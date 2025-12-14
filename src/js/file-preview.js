/**
 * File Preview Overlay
 * Shows a preview of files before sending with options like compression for images
 */

let filePreviewOverlay = null;
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

// Image extensions supported by the image crate
const SUPPORTED_IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'tiff', 'tif', 'ico'];

// Video extensions supported for preview (mp4, webm, mov - except on Linux)
const SUPPORTED_VIDEO_EXTENSIONS = ['mp4', 'webm', 'mov'];

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
 * Get file extension from path
 * @param {string} filepath - File path
 * @returns {string} File extension (lowercase)
 */
function getFileExtension(filepath) {
    const parts = filepath.split('.');
    return parts.length > 1 ? parts.pop().toLowerCase() : '';
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
    
    return 'icon-file';
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
                    <div class="file-preview-name" id="file-preview-name"></div>
                    <div class="file-preview-details">
                        <span class="file-preview-detail" id="file-preview-size"></span>
                    </div>
                </div>
                <div class="file-preview-options" id="file-preview-options"></div>
            </div>
            <div class="file-preview-buttons">
                <button class="file-preview-btn file-preview-btn-cancel" id="file-preview-cancel">Cancel</button>
                <button class="file-preview-btn file-preview-btn-send" id="file-preview-send">Send</button>
            </div>
        </div>
    `;
    
    document.body.appendChild(filePreviewOverlay);
    
    // Event listeners
    document.getElementById('file-preview-cancel').addEventListener('click', closeFilePreview);
    document.getElementById('file-preview-send').addEventListener('click', sendPreviewedFile);
    
    // Close on background click
    filePreviewOverlay.addEventListener('click', (e) => {
        if (e.target === filePreviewOverlay) {
            closeFilePreview();
        }
    });
    
    // Close on Escape key
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && filePreviewOverlay && filePreviewOverlay.classList.contains('active')) {
            closeFilePreview();
        }
    });
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
    
    // Update file name
    document.getElementById('file-preview-name').textContent = fileName;
    
    // Update file size
    document.getElementById('file-preview-size').textContent = formatFileSize(fileSize);
    
    // Update content area
    const contentArea = document.getElementById('file-preview-content');
    
    if (isImage) {
        // Show image preview
        const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
        
        if (isAndroid && androidPreview) {
            // On Android, use the base64 preview we already got from cache_android_file
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${androidPreview}" class="file-preview-image" alt="Preview">
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
        // Show video preview (not supported on Android with content URIs)
        const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
        
        if (isAndroid) {
            // On Android, just show video icon since video preview doesn't work with content URIs
            contentArea.innerHTML = `
                <div class="file-preview-icon-container">
                    <div class="icon icon-video file-preview-icon"></div>
                </div>
            `;
        } else {
            const videoSrc = convertFileSrc(filepath);
            contentArea.innerHTML = `
                <div class="file-preview-video-container">
                    <video src="${videoSrc}" class="file-preview-video" controls muted></video>
                </div>
            `;
        }
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
    const MIN_COMPRESS_SIZE = 25 * 1024; // 25KB
    const isGif = ext === 'gif';
    if (isImage && !isGif && fileSize > MIN_COMPRESS_SIZE) {
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
    
    // Show overlay
    filePreviewOverlay.style.display = 'flex';
    setTimeout(() => filePreviewOverlay.classList.add('active'), 10);
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
    
    // Store the File object for later use when sending
    pendingFileObject = file;
    pendingFileBytes = null; // Clear bytes mode - we're using File object
    pendingFileName = fileName;
    pendingFileExt = ext;
    pendingFile = null; // Clear file path since we're using File object
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    
    // Update file name
    document.getElementById('file-preview-name').textContent = fileName;
    
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
    
    if (isImage) {
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
    
    // Mark that we're using bytes mode (no file path)
    pendingFileBytes = true; // Flag to indicate bytes mode
    pendingFileObject = null; // Clear File object since we're using cached bytes
    pendingFileName = fileName;
    pendingFileExt = ext;
    pendingFile = null; // Clear file path since we're using bytes
    pendingReceiver = receiver;
    pendingReplyRef = replyRef;
    
    // Update file name
    document.getElementById('file-preview-name').textContent = fileName;
    
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
    
    if (isImage) {
        // Use the preview from Rust (already a base64 data URL)
        if (preview) {
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${preview}" class="file-preview-image" alt="Preview">
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
    const infoElement = document.getElementById('file-preview-compress-info');
    
    try {
        // Read file bytes
        const arrayBuffer = await file.arrayBuffer();
        const bytes = Array.from(new Uint8Array(arrayBuffer));
        
        // Cache bytes in Rust - this also generates a base64 preview for images
        const result = await invoke('cache_file_bytes', {
            bytes: bytes,
            fileName: fileName,
            extension: ext
        });
        
        // Display the preview from Rust (base64 data URL - works on all Android versions)
        if (result.preview) {
            contentArea.innerHTML = `
                <div class="file-preview-image-container">
                    <img src="${result.preview}" class="file-preview-image" alt="Preview">
                </div>
            `;
        } else {
            // Fallback to icon if no preview
            contentArea.innerHTML = `
                <div class="file-preview-icon-container">
                    <div class="icon icon-image file-preview-icon"></div>
                </div>
            `;
        }
        
        // Start compression if requested
        if (startCompression) {
            compressionInProgress = true;
            compressionComplete = false;
            
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
        }
    } catch (e) {
        console.error('Failed to cache file:', e);
        // Show error state
        contentArea.innerHTML = `
            <div class="file-preview-icon-container">
                <div class="icon icon-image file-preview-icon"></div>
            </div>
        `;
        if (infoElement) {
            infoElement.textContent = 'Failed to load';
        }
        compressionInProgress = false;
    }
}

/**
 * Close file preview overlay
 */
function closeFilePreview() {
    if (!filePreviewOverlay) return;
    
    const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
    
    // Stop polling
    if (compressionPollingInterval) {
        clearInterval(compressionPollingInterval);
        compressionPollingInterval = null;
    }
    
    // Clear caches if we're cancelling
    if (pendingFile) {
        invoke('clear_compression_cache', { filePath: pendingFile }).catch(() => {});
        // On Android, also clear the file cache since we cached the bytes
        if (isAndroid) {
            invoke('clear_android_file_cache', { filePath: pendingFile }).catch(() => {});
        }
    }
    if (pendingFileBytes || pendingFileObject) {
        // Clear cached bytes (used for clipboard paste or File object compression)
        invoke('clear_cached_file').catch(() => {});
    }
    
    // Stop and clean up any video element
    const video = filePreviewOverlay.querySelector('video');
    if (video) {
        video.pause();
        video.src = '';
        video.load();
    }
    
    // Clean up any blob URLs
    const contentArea = document.getElementById('file-preview-content');
    if (contentArea && contentArea.dataset.blobUrl) {
        URL.revokeObjectURL(contentArea.dataset.blobUrl);
        delete contentArea.dataset.blobUrl;
    }
    
    filePreviewOverlay.classList.remove('active');
    setTimeout(() => {
        filePreviewOverlay.style.display = 'none';
        // Clear content after animation
        if (contentArea) {
            contentArea.innerHTML = '';
        }
    }, 200);
    
    // Reset state
    compressionInProgress = false;
    compressionComplete = false;
    pendingFile = null;
    pendingFileBytes = null;
    pendingFileObject = null;
    pendingFileName = null;
    pendingFileExt = null;
    pendingReceiver = null;
    pendingReplyRef = null;
}

/**
 * Send the previewed file
 */
async function sendPreviewedFile() {
    const isAndroid = typeof platformFeatures !== 'undefined' && platformFeatures.os === 'android';
    
    // Check if we have either a file path, cached bytes, or File object
    if ((!pendingFile && !pendingFileBytes && !pendingFileObject) || !pendingReceiver) return;
    
    // Capture values before closing
    const filePath = pendingFile;
    const usingBytes = pendingFileBytes; // Flag indicating bytes mode
    const fileObject = pendingFileObject; // File object for Android optimized flow
    const receiver = pendingReceiver;
    const replyRef = pendingReplyRef;
    const ext = pendingFileExt;
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
    pendingReceiver = null;
    pendingReplyRef = null;
    compressionInProgress = false;
    compressionComplete = false;
    
    // Send file in background
    try {
        if (fileObject) {
            // Android File object flow
            // If compression was enabled and we have cached bytes, use them
            // Otherwise, read from File object directly
            if (shouldCompress || compressionWasStarted) {
                // Use cached bytes (compression was started, so bytes are cached)
                await invoke("send_cached_file", {
                    receiver: receiver,
                    repliedTo: replyRef,
                    useCompression: shouldCompress
                });
            } else {
                // No compression needed and bytes weren't cached, read directly
                const arrayBuffer = await fileObject.arrayBuffer();
                const bytes = Array.from(new Uint8Array(arrayBuffer));
                await invoke("send_file_bytes", {
                    receiver: receiver,
                    repliedTo: replyRef,
                    fileBytes: bytes,
                    fileName: fileObject.name,
                    useCompression: false
                });
            }
        } else if (usingBytes) {
            // Legacy flow: use cached bytes from JS (clipboard paste)
            await invoke("send_cached_file", {
                receiver: receiver,
                repliedTo: replyRef,
                useCompression: shouldCompress
            });
        } else if (shouldCompress) {
            // Desktop: use cached compressed file (will wait if still compressing)
            await invoke("send_cached_compressed_file", {
                receiver: receiver,
                repliedTo: replyRef,
                filePath: filePath
            });
        } else {
            // Desktop: send without compression, but clear the cache first
            await invoke("clear_compression_cache", { filePath: filePath });
            await invoke("file_message", {
                receiver: receiver,
                repliedTo: replyRef,
                filePath: filePath
            });
        }
    } catch (e) {
        console.error('Failed to send file:', e);
        popupConfirm(e.toString(), '', true, '', 'vector_warning.svg');
    }
    
    nLastTypingIndicator = 0;
}