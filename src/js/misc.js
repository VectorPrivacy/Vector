/**
 * Shows a simple toast notification
 * @param {string} message - The message to display
 */
function showToast(message, persist = false) {
    // Create toast element if it doesn't exist
    let toast = document.getElementById('pivx-toast');
    if (!toast) {
        toast = document.createElement('div');
        toast.id = 'pivx-toast';
        toast.style.cssText = `
            position: fixed;
            bottom: 80px;
            left: 50%;
            transform: translateX(-50%);
            background: rgba(0, 0, 0, 0.8);
            backdrop-filter: blur(20px);
            -webkit-backdrop-filter: blur(10px);
            border: 1px solid var(--toast-border-color, #161616);
            box-shadow:
            0 0 4px rgba(0, 0, 0, 0.8),
            0 0 12px rgba(0, 0, 0, 0.6),
            0 0 30px rgba(0, 0, 0, 0.4);
            color: white;
            padding: 12px 24px;
            border-radius: 8px;
            z-index: 10000;
            font-size: 14px;
            opacity: 0;
            transition: opacity 0.3s ease;
            pointer-events: none;
        `;
        document.body.appendChild(toast);
    }

    let backdrop = document.getElementById('toast-backdrop');
    if (!backdrop) {
    backdrop = document.createElement('div');
    backdrop.id = 'toast-backdrop';
    backdrop.style.cssText = `
        position:fixed;
        top:0;
        left:0;
        width:100%;
        height:100%;
        background:linear-gradient(to bottom, rgba(0,0,0,0) 0%, rgba(0,0,0,0) 50%, rgba(0,0,0,0.8) 100%);
        opacity:0;
        z-index:9999;pointer-events:none;
        transition:opacity 0.3s ease;
    `;
    document.body.appendChild(backdrop);
    }
    toast.textContent = message;
    toast.style.opacity = '1';
    backdrop.style.opacity = '1';
    clearTimeout(toast._timeout);

    // Persistent toasts stay until hideToast() — for awaits that can outlast the auto-timeout
    // (e.g. a multi-second relay fetch), so feedback doesn't vanish mid-operation.
    if (persist) return;

    // Scale duration by message length: 1.5s base + 40ms per char, capped at 6s
    const duration = Math.min(1500 + message.length * 40, 6000);
    toast._timeout = setTimeout(() => {
        backdrop.style.opacity = '0';
        toast.style.opacity = '0';
    }, duration);
}

/** Dismiss the shared toast (pairs with showToast(msg, true)). */
function hideToast() {
    const toast = document.getElementById('pivx-toast');
    const backdrop = document.getElementById('toast-backdrop');
    if (toast) { clearTimeout(toast._timeout); toast.style.opacity = '0'; }
    if (backdrop) backdrop.style.opacity = '0';
}

/**
 * Generate a placeholder avatar
 * @param {boolean} isGroup - Whether this is a group chat avatar
 * @param {number} limitSizeTo - An optional pixel width/height to lock the avatar to
 */
function createPlaceholderAvatar(isGroup = false, limitSizeTo = null) {
    // Create avatar container with the appropriate placeholder SVG
    const divAvatar = document.createElement('div');
    divAvatar.classList.add('placeholder-avatar');
    if (limitSizeTo) {
        divAvatar.style.minHeight = limitSizeTo + 'px';
        divAvatar.style.minWidth = limitSizeTo + 'px';
        divAvatar.style.maxHeight = limitSizeTo + 'px';
        divAvatar.style.maxWidth = limitSizeTo + 'px';
    }

    // Use the appropriate placeholder SVG based on chat type
    divAvatar.style.backgroundImage = `url("${isGroup ? 'icons/group-placeholder.svg' : 'icons/user-placeholder.svg'}")`;
    divAvatar.style.backgroundSize = 'cover';
    divAvatar.style.backgroundPosition = 'center';

    return divAvatar;
}

/**
 * Show a popup dialog to confirm an action.
 *
 * @param {String} strTitle - The title of the popup dialog.
 * @param {String} strSubtext - The subtext of the popup dialog.
 * @param {Boolean} fNotice - If this is a Notice or an Interactive Dialog.
 * @param {String} strInputPlaceholder - If specified, renders a text input with a custom placeholder, and returns a string instead of a boolean.
 * @param {String} strIcon - If specified, an icon to be displayed above the popup.
 * @param {String} strTitleClass - If specified, a CSS class to be added to the title element (e.g., 'typing-indicator-text').
 * @return {Promise<Boolean>} - The Promise will resolve to 'true' if confirm button was clicked, otherwise 'false'.
 */
/**
 * Take over the screen when this build is older than the account's database.
 *
 * Deliberately terminal: there is no dismiss path, because continuing would let
 * an older schema write over a newer one and corrupt message history.
 *
 * @param {Object} info - vector-core's DowngradeBlock: { db_schema, supported_schema, last_app_version }
 */
async function showDowngradeBlock(info) {
    const domBlock = document.getElementById('downgrade-block');
    if (!domBlock) return;

    const format = (v) => (typeof parseVersion === 'function' ? parseVersion(v).display : `v${v}`);

    let current = 'This build';
    try {
        current = format(await window.__TAURI__.app.getVersion());
    } catch (e) { /* keep the generic label */ }

    // The version that wrote the database reads far better than a schema
    // number; the number is only reachable if an older build stamped nothing.
    const required = info?.last_app_version
        ? format(info.last_app_version)
        : 'A newer version';

    document.getElementById('downgrade-current').textContent = current;
    document.getElementById('downgrade-required').textContent = required;

    document.getElementById('downgrade-get-latest').onclick = () => openUrl('https://vectorapp.io');
    document.getElementById('downgrade-quit').onclick = async () => {
        // The process plugin is desktop-only, so this is accessed lazily.
        try {
            await window.__TAURI__.process.exit(0);
        } catch (e) {
            window.close();
        }
    };

    domBlock.style.display = 'flex';
}

async function popupConfirm(strTitle, strSubtext, fNotice = false, strInputPlaceholder = '', strIcon = '', strTitleClass = '', strConfirmText = null, fCircularIcon = false) {
    // Display the popup and render the UI
    domPopup.style.display = '';
    // A bare filename resolves under ./icons/; a full URL (asset/blob/data/http, e.g. a
    // decrypted community logo) is used verbatim.
    domPopupIcon.src = /:\/\/|^data:|^blob:/.test(strIcon) ? strIcon : './icons/' + strIcon;
    domPopupIcon.style.display = strIcon ? '' : 'none';
    // Avatar-style crop for community logos (matches how Vector renders all avatars).
    domPopupIcon.classList.toggle('popup-icon-circular', fCircularIcon);
    domPopupTitle.innerText = strTitle;
    // Clear any previous classes and add the new one if specified
    domPopupTitle.className = strTitleClass;
    domPopupSubtext.innerHTML = strSubtext;

    // Show the backdrop by adding the active class
    domApp.classList.add('active');

    // Adjust the 'Confirm' button. Caller-provided label wins; otherwise
    // default to 'Okay' for notices, 'Confirm' for confirms.
    domPopupConfirmBtn.innerText = strConfirmText || (fNotice ? 'Okay' : 'Confirm');
    domPopupCancelBtn.style.display = fNotice ? 'none' : '';

    // If a string placeholder is specified, render it
    domPopupInput.value = '';
    if (strInputPlaceholder) {
        domPopupInput.style.display = '';
        domPopupInput.setAttribute('placeholder', strInputPlaceholder);
        domPopupInput.focus();
    } else {
        // Otherwise, hide it
        domPopupInput.style.display = 'none';
    }

    // Resolve once, detaching every listener we attached. The handlers are NAMED so
    // removeEventListener actually unhooks them — registering anonymous wrappers and
    // trying to remove a different reference leaks one listener per popup onto the
    // shared confirm/cancel buttons (stale handlers then double-fire on later popups).
    return new Promise((resolve) => {
        const confirmValue = () => (strInputPlaceholder ? domPopupInput.value : true);
        const cleanup = () => {
            domPopupConfirmBtn.removeEventListener('click', onConfirm);
            domPopupCancelBtn.removeEventListener('click', onCancel);
            document.removeEventListener('keydown', onKeyDown);
        };
        const finish = (value) => {
            cleanup();
            domPopup.style.display = 'none';
            domApp.classList.remove('active');
            resolve(value);
        };
        const onConfirm = () => { popBack('popup-confirm'); finish(confirmValue()); };
        const onCancel = () => { popBack('popup-confirm'); finish(false); };
        const onKeyDown = (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                popBack('popup-confirm');
                finish(confirmValue());
            } else if (e.key === 'Escape') {
                e.preventDefault();
                popBack('popup-confirm');
                finish(fNotice ? confirmValue() : false);
            }
        };

        document.addEventListener('keydown', onKeyDown);
        domPopupConfirmBtn.addEventListener('click', onConfirm);
        if (!fNotice) domPopupCancelBtn.addEventListener('click', onCancel);

        // Hardware-back: notices accept-on-back (matches Escape), confirms cancel-on-back.
        // This fires as a RESULT of back, so it must not popBack again — just resolve.
        pushBack('popup-confirm', () => finish(fNotice ? confirmValue() : false));
    });
}

/** Helper function to determine if a date is today */
function isToday(date) {
    const today = new Date();
    return date.getDate() === today.getDate() &&
           date.getMonth() === today.getMonth() &&
           date.getFullYear() === today.getFullYear();
}

/** Helper function to determine if a date is yesterday */
function isYesterday(date) {
    const yesterday = new Date();
    yesterday.setDate(yesterday.getDate() - 1);
    return date.getDate() === yesterday.getDate() &&
           date.getMonth() === yesterday.getMonth() &&
           date.getFullYear() === yesterday.getFullYear();
}

/**
 * Calculates time elapsed since a given timestamp and returns a human-readable string.
 * @param {number|string|Date} timestamp - The timestamp to compare against current time
 * @returns {string} A human-readable string representing time elapsed (e.g., "Now", "1 min", "2 hours")
 */
function timeAgo(timestamp) {
    // Convert timestamp to Date object if it's not already
    const pastDate = timestamp instanceof Date ? timestamp : new Date(timestamp);
    const now = new Date();

    // Calculate time difference in milliseconds
    const diffMs = now - pastDate;

    // Convert to seconds
    const diffSec = Math.floor(diffMs / 1000);

    // Less than a minute
    if (diffSec < 60) {
        return "Now";
    }

    // Minutes (less than an hour)
    if (diffSec < 3600) {
        const mins = Math.floor(diffSec / 60);
        return `${mins}m`;
    }

    // Hours (less than a day)
    if (diffSec < 86400) {
        const hours = Math.floor(diffSec / 3600);
        return `${hours}h`;
    }

    // Days (less than a week)
    if (diffSec < 604800) {
        const days = Math.floor(diffSec / 86400);
        return `${days}d`;
    }

    // Weeks (less than a month - approximated as 30 days)
    if (diffSec < 2592000) {
        const weeks = Math.floor(diffSec / 604800);
        return `${weeks}w`;
    }

    // Months (less than a year)
    if (diffSec < 31536000) {
        const months = Math.floor(diffSec / 2592000);
        return `${months}mo`;
    }

    // Years
    const years = Math.floor(diffSec / 31536000);
    return `${years}y`;
}

/** 
 * Scroll to the bottom of a scrollable element
 * @param {HTMLElement} domElement - The DOM element to scroll
 * @param {boolean} [fSmooth=true] - Whether to use smooth scrolling (true) or instant scrolling (false)
 */
function scrollToBottom(domElement, fSmooth = true) {
    // Mark this as an app-initiated scroll so the chat's intent-aware pin doesn't
    // misread it as the user moving. No-op for non-chat scrollables (the guard
    // only matters for #chat-messages); harmless elsewhere.
    if (typeof beginProgrammaticScroll === 'function') beginProgrammaticScroll();
    domElement.scrollTo({
        top: domElement.scrollHeight,
        behavior: fSmooth ? 'smooth' : 'auto'
    });
}

/**
 * Creates a scroll handler that shows/hides a button based on scroll position within a div
 * @param {HTMLElement} scrollableDiv - The div element that has scrollable content
 * @param {HTMLElement} bottomButton - The button element to show/hide
 * @param {Object} [options] - Configuration options
 * @param {number} [options.threshold=250] - Scroll threshold in pixels from bottom to trigger button visibility
 * @param {number} [options.throttleTime=150] - Throttle time in milliseconds
 * @param {boolean} [options.smoothScroll=true] - Whether to use smooth scrolling
 * @returns {Function} Cleanup function to remove event listeners
 */
// Set by createScrollHandler to its current un-throttled visibility evaluator,
// so window slides / renders can re-evaluate the scroll-return button without a
// scroll event. No-op until a handler is wired.
let _scrollReturnReeval = null;
/** Re-evaluate the scroll-return button's visibility now (called after window
 *  slides, renders, and drops where no scroll event fires). */
function refreshScrollReturnButton() { if (_scrollReturnReeval) _scrollReturnReeval(); }

function createScrollHandler(scrollableDiv, bottomButton, options = {}) {
    const SCROLL_THRESHOLD = options.threshold ?? 250;
    const THROTTLE_TIME = options.throttleTime ?? 150;
    const SMOOTH_SCROLL = options.smoothScroll ?? true;
    // Optional override: when this returns true the button must stay hidden
    // regardless of the distance-from-bottom check. Used to suppress the
    // "scroll down" arrow during chat-open reflow, when content is still
    // settling and the user hasn't actually scrolled away.
    const isPinned = typeof options.isPinned === 'function' ? options.isPinned : null;
    // Optional onClick — called instead of (well, alongside) the default
    // scroll-to-bottom so callers can clear unread badges, etc.
    const onClick = typeof options.onClick === 'function' ? options.onClick : null;
    // Optional force-visible predicate: when it returns true the button stays
    // shown regardless of the DOM scroll-distance check. With DOM windowing the
    // rendered bottom isn't the data bottom (newer rows live below the window),
    // so distance-from-bottom alone can't tell the button to stay visible.
    const shouldForceVisible = typeof options.shouldForceVisible === 'function' ? options.shouldForceVisible : null;
    // Mirror of shouldForceVisible: when windowed AND the live tail is on screen, the button must
    // HIDE regardless of pin/scroll-position (a media-reflow scroll could otherwise strand it visible).
    const shouldForceHidden = typeof options.shouldForceHidden === 'function' ? options.shouldForceHidden : null;
    // Optional jump-to-bottom override: returns true if it handled landing at
    // the data bottom (e.g. re-rendering the newest window), suppressing the
    // default scrollTo(scrollHeight) which can't reach the live tail when windowed.
    const onJumpToBottom = typeof options.onJumpToBottom === 'function' ? options.onJumpToBottom : null;

    /**
     * Throttles a function call
     * @param {Function} func - Function to throttle
     * @param {number} limit - Milliseconds to wait between calls
     * @returns {Function} Throttled function
     */
    function throttle(func, limit) {
        let inThrottle;
        return function(...args) {
            if (!inThrottle) {
                func.apply(this, args);
                inThrottle = true;
                setTimeout(() => inThrottle = false, limit);
            }
        };
    }

    /**
     * Handles the scroll event and updates button visibility
     * @private
     */
    // Core visibility decision, shared by the scroll listener and by callers
    // that slide the window (which fire no scroll event). Force-visible wins;
    // pinned hides; otherwise distance-from-bottom decides.
    const evaluateVisibility = () => {
        if (shouldForceVisible && shouldForceVisible()) {
            bottomButton.classList.add('visible');
            return;
        }
        if (shouldForceHidden && shouldForceHidden()) {
            bottomButton.classList.remove('visible');
            return;
        }
        if (isPinned && isPinned()) {
            bottomButton.classList.remove('visible');
            return;
        }
        const currentScrollTop = scrollableDiv.scrollTop;
        const maxScroll = scrollableDiv.scrollHeight - scrollableDiv.clientHeight;
        const distanceFromBottom = maxScroll - currentScrollTop;

        if (distanceFromBottom > SCROLL_THRESHOLD) {
            bottomButton.classList.add('visible');
        } else {
            bottomButton.classList.remove('visible');
        }
    };
    const handleScroll = throttle(evaluateVisibility, THROTTLE_TIME);
    // Expose an un-throttled re-evaluation hook so window slides / renders can
    // refresh the button the instant the rendered range changes.
    _scrollReturnReeval = evaluateVisibility;

    /**
     * Scrolls to bottom and hides the button
     * @private
     */
    const handleButtonClick = () => {
        // When windowed and scrolled/windowed away from the live tail, the DOM
        // bottom isn't the data bottom — a plain scrollTo would land on stale
        // rows. Let the caller re-render the newest window + pin instead.
        if (onJumpToBottom && onJumpToBottom()) {
            bottomButton.classList.remove('visible');
            if (onClick) onClick();
            return;
        }
        scrollToBottom(scrollableDiv, SMOOTH_SCROLL);
        bottomButton.classList.remove('visible');
        if (onClick) onClick();
    };

    // Add event listeners
    scrollableDiv.addEventListener('scroll', handleScroll);
    bottomButton.addEventListener('click', handleButtonClick);
    
    return () => {
        scrollableDiv.removeEventListener('scroll', handleScroll);
        bottomButton.removeEventListener('click', handleButtonClick);
    };
}

/**
 * Smoothly scrolls an Element into the center of its container view.
 * 
 * @param {HTMLElement} targetMessage - The element to center in view
 */
function centerInView(targetMessage) {
    // Get the container and the target message
    const container = targetMessage.parentElement;

    // Get the container's height
    const containerHeight = container.clientHeight;

    // Calculate the scroll position needed to center the message
    const scrollPosition = targetMessage.offsetTop - (containerHeight / 2) + (targetMessage.offsetHeight / 2);

    // App-initiated jump (reply-jump / unread-jump centering) — don't let the
    // chat's intent-aware pin read this as the user scrolling.
    if (typeof beginProgrammaticScroll === 'function') beginProgrammaticScroll();
    // Smooth scroll to the calculated position
    container.scrollTo({
        top: scrollPosition,
        behavior: 'smooth'
    });
}

function setAsyncInterval(callback, interval) {
    let timer = null;
    async function run() {
        while (true) {
            await new Promise(resolve => timer = setTimeout(resolve, interval));
            try {
                await callback();
            } catch (e) {
                console.error('[setAsyncInterval] callback error:', e);
            }
        }
    }
    run();
    return {
        clear: () => clearTimeout(timer)
    };
}

/**
 * Formats a number of bytes into a human-readable string with appropriate units.
 * 
 * @param {number} bytes - The number of bytes to format.
 * @param {number} [decimals=2] - The number of decimal places to include in the formatted output.
 * @returns {string} A formatted string representing the bytes in human-readable form.
 */
function formatBytes(bytes, decimals = 2, pad = false) {
    if (bytes === 0) return '0 Bytes';

    const units = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB'];
    let unitIndex = 0;
    let value = bytes;

    while (value >= 1024 && unitIndex < units.length - 1) {
      value /= 1024;
      unitIndex++;
    }

    const fixed = value.toFixed(decimals);
    return (pad ? fixed : fixed.replace(/\.0+$|(\.[0-9]*[1-9])0+$/, '$1')) + ' ' + units[unitIndex];
  }

/**
 * Gets information about a file type based on its extension.
 * @param {string} extension - The file extension (e.g., 'jpg', 'mp4', 'pdf')
 * @returns {Object} An object containing information about the file type
 */
function getFileTypeInfo(extension) {
    // Define file types with descriptions and appropriate icons
    const fileTypes = {
      // Images
      "png": { description: "Picture", icon: "image" },
      "jpg": { description: "Picture", icon: "image" },
      "jpeg": { description: "Picture", icon: "image" },
      "gif": { description: "GIF Animation", icon: "film" },
      "webp": { description: "Picture", icon: "image" },
      "svg": { description: "Vector Image", icon: "image" },
      "bmp": { description: "Bitmap Image", icon: "image" },
      "ico": { description: "Icon", icon: "image" },
      "tiff": { description: "TIFF Image", icon: "image" },
      "tif": { description: "TIFF Image", icon: "image" },
      
      // Raw Images
      "raw": { description: "RAW Image", icon: "image" },
      "dng": { description: "RAW Image", icon: "image" },
      "cr2": { description: "Canon RAW", icon: "image" },
      "nef": { description: "Nikon RAW", icon: "image" },
      "arw": { description: "Sony RAW", icon: "image" },
      "orf": { description: "Olympus RAW", icon: "image" },
      "rw2": { description: "Panasonic RAW", icon: "image" },

      // Audio
      "wav": { description: "Voice Message", icon: "mic-on" },
      "mp3": { description: "Audio Clip", icon: "mic-on" },
      "m4a": { description: "Audio Clip", icon: "mic-on" },
      "aac": { description: "Audio Clip", icon: "mic-on" },
      "flac": { description: "Audio Clip", icon: "mic-on" },
      "ogg": { description: "Audio Clip", icon: "mic-on" },
      "wma": { description: "Audio Clip", icon: "mic-on" },
      "opus": { description: "Audio Clip", icon: "mic-on" },
      "ape": { description: "Audio Clip", icon: "mic-on" },
      "wv": { description: "Audio Clip", icon: "mic-on" },
      
      // Audio Project Files
      "aup": { description: "Audacity Project", icon: "mic-on" },
      "flp": { description: "FL Studio Project", icon: "mic-on" },
      "als": { description: "Ableton Project", icon: "mic-on" },
      "logic": { description: "Logic Project", icon: "mic-on" },
      "band": { description: "GarageBand Project", icon: "mic-on" },

      // Videos
      "mp4": { description: "Video", icon: "film" },
      "webm": { description: "Video", icon: "film" },
      "mov": { description: "Video", icon: "film" },
      "avi": { description: "Video", icon: "film" },
      "mkv": { description: "Video", icon: "film" },
      "flv": { description: "Flash Video", icon: "film" },
      "wmv": { description: "Windows Video", icon: "film" },
      "mpg": { description: "MPEG Video", icon: "film" },
      "mpeg": { description: "MPEG Video", icon: "film" },
      "m4v": { description: "MPEG-4 Video", icon: "film" },
      "3gp": { description: "3GP Video", icon: "film" },
      "3g2": { description: "3G2 Video", icon: "film" },
      "f4v": { description: "Flash MP4 Video", icon: "film" },
      "asf": { description: "Advanced Systems Format", icon: "film" },
      "rm": { description: "RealMedia", icon: "film" },
      "vob": { description: "DVD Video", icon: "film" },
      "ogv": { description: "Ogg Video", icon: "film" },
      "mxf": { description: "Material Exchange Format", icon: "film" },
      "ts": { description: "MPEG Transport Stream", icon: "film" },
      "m2ts": { description: "Blu-ray Video", icon: "film" },
      
      // Documents
      "pdf": { description: "PDF Document", icon: "file" },
      "doc": { description: "Word Document", icon: "file" },
      "docx": { description: "Word Document", icon: "file" },
      "xls": { description: "Excel Spreadsheet", icon: "file" },
      "xlsx": { description: "Excel Spreadsheet", icon: "file" },
      "ppt": { description: "PowerPoint Presentation", icon: "file" },
      "pptx": { description: "PowerPoint Presentation", icon: "file" },
      "odt": { description: "OpenDocument Text", icon: "file" },
      "ods": { description: "OpenDocument Spreadsheet", icon: "file" },
      "odp": { description: "OpenDocument Presentation", icon: "file" },
      "rtf": { description: "Rich Text Document", icon: "file" },
      "tex": { description: "LaTeX Document", icon: "file" },
      "pages": { description: "Pages Document", icon: "file" },
      "numbers": { description: "Numbers Spreadsheet", icon: "file" },
      "key": { description: "Keynote Presentation", icon: "file" },
      
      // Text Files
      "txt": { description: "Text File", icon: "file" },
      "md": { description: "Markdown File", icon: "file" },
      "log": { description: "Log File", icon: "file" },
      "csv": { description: "CSV File", icon: "file" },
      "tsv": { description: "TSV File", icon: "file" },
      
      // Data Files
      "json": { description: "JSON File", icon: "file" },
      "xml": { description: "XML File", icon: "file" },
      "yaml": { description: "YAML File", icon: "file" },
      "yml": { description: "YAML File", icon: "file" },
      "toml": { description: "TOML File", icon: "file" },
      "sql": { description: "SQL File", icon: "file" },
      "db": { description: "Database File", icon: "file" },
      "sqlite": { description: "SQLite Database", icon: "file" },
      
      // Archives
      "zip": { description: "ZIP Archive", icon: "folder" },
      "rar": { description: "RAR Archive", icon: "folder" },
      "7z": { description: "7-Zip Archive", icon: "folder" },
      "tar": { description: "TAR Archive", icon: "folder" },
      "gz": { description: "GZip Archive", icon: "folder" },
      "bz2": { description: "BZip2 Archive", icon: "folder" },
      "xz": { description: "XZ Archive", icon: "folder" },
      "tgz": { description: "Compressed TAR", icon: "folder" },
      "tbz": { description: "Compressed TAR", icon: "folder" },
      "txz": { description: "Compressed TAR", icon: "folder" },
      "cab": { description: "Cabinet Archive", icon: "folder" },
      "iso": { description: "Disc Image", icon: "file" },
      "dmg": { description: "macOS Disk Image", icon: "file" },
      "pkg": { description: "Package File", icon: "file" },
      "deb": { description: "Debian Package", icon: "file" },
      "rpm": { description: "RPM Package", icon: "file" },
      "apk": { description: "Android Package", icon: "file" },
      "ipa": { description: "iOS App", icon: "file" },
      "jar": { description: "Java Archive", icon: "file" },
      "war": { description: "Web Archive", icon: "file" },
      "ear": { description: "Enterprise Archive", icon: "file" },
      
      // 3D Files
      "obj": { description: "3D Object", icon: "file" },
      "fbx": { description: "Autodesk FBX", icon: "file" },
      "gltf": { description: "GL Transmission Format", icon: "file" },
      "glb": { description: "GL Binary", icon: "file" },
      "stl": { description: "Stereolithography", icon: "file" },
      "ply": { description: "Polygon File", icon: "file" },
      "dae": { description: "COLLADA", icon: "file" },
      "3ds": { description: "3D Studio", icon: "file" },
      "blend": { description: "Blender File", icon: "file" },
      "c4d": { description: "Cinema 4D", icon: "file" },
      "max": { description: "3ds Max", icon: "file" },
      "ma": { description: "Maya ASCII", icon: "file" },
      "mb": { description: "Maya Binary", icon: "file" },
      "usdz": { description: "Universal Scene", icon: "file" },
      
      // CAD Files
      "dwg": { description: "AutoCAD Drawing", icon: "file" },
      "dxf": { description: "Drawing Exchange", icon: "file" },
      "step": { description: "STEP CAD", icon: "file" },
      "stp": { description: "STEP CAD", icon: "file" },
      "iges": { description: "IGES CAD", icon: "file" },
      "igs": { description: "IGES CAD", icon: "file" },
      "sat": { description: "ACIS SAT", icon: "file" },
      "ipt": { description: "Inventor Part", icon: "file" },
      "iam": { description: "Inventor Assembly", icon: "file" },
      "prt": { description: "Part File", icon: "file" },
      "sldprt": { description: "SolidWorks Part", icon: "file" },
      "sldasm": { description: "SolidWorks Assembly", icon: "file" },
      "slddrw": { description: "SolidWorks Drawing", icon: "file" },
      "catpart": { description: "CATIA Part", icon: "file" },
      "catproduct": { description: "CATIA Product", icon: "file" },
      
      // Code Files
      "js": { description: "JavaScript", icon: "file" },
      "ts": { description: "TypeScript", icon: "file" },
      "jsx": { description: "React JSX", icon: "file" },
      "tsx": { description: "React TSX", icon: "file" },
      "py": { description: "Python", icon: "file" },
      "rs": { description: "Rust", icon: "file" },
      "go": { description: "Go", icon: "file" },
      "java": { description: "Java", icon: "file" },
      "kt": { description: "Kotlin", icon: "file" },
      "cpp": { description: "C++", icon: "file" },
      "cc": { description: "C++", icon: "file" },
      "cxx": { description: "C++", icon: "file" },
      "c": { description: "C", icon: "file" },
      "h": { description: "Header File", icon: "file" },
      "hpp": { description: "C++ Header", icon: "file" },
      "cs": { description: "C#", icon: "file" },
      "rb": { description: "Ruby", icon: "file" },
      "php": { description: "PHP", icon: "file" },
      "swift": { description: "Swift", icon: "file" },
      "m": { description: "Objective-C", icon: "file" },
      "mm": { description: "Objective-C++", icon: "file" },
      "lua": { description: "Lua", icon: "file" },
      "r": { description: "R Script", icon: "file" },
      "scala": { description: "Scala", icon: "file" },
      "clj": { description: "Clojure", icon: "file" },
      "dart": { description: "Dart", icon: "file" },
      "ex": { description: "Elixir", icon: "file" },
      "elm": { description: "Elm", icon: "file" },
      "erl": { description: "Erlang", icon: "file" },
      "fs": { description: "F#", icon: "file" },
      "hs": { description: "Haskell", icon: "file" },
      "jl": { description: "Julia", icon: "file" },
      "nim": { description: "Nim", icon: "file" },
      "pl": { description: "Perl", icon: "file" },
      "sh": { description: "Shell Script", icon: "file" },
      "bash": { description: "Bash Script", icon: "file" },
      "zsh": { description: "Zsh Script", icon: "file" },
      "fish": { description: "Fish Script", icon: "file" },
      "ps1": { description: "PowerShell", icon: "file" },
      "bat": { description: "Batch File", icon: "file" },
      "cmd": { description: "Command File", icon: "file" },
      "vb": { description: "Visual Basic", icon: "file" },
      "vbs": { description: "VBScript", icon: "file" },
      "asm": { description: "Assembly", icon: "file" },
      "s": { description: "Assembly", icon: "file" },
      
      // Config Files
      "ini": { description: "INI Config", icon: "file" },
      "cfg": { description: "Config File", icon: "file" },
      "conf": { description: "Config File", icon: "file" },
      "config": { description: "Config File", icon: "file" },
      "env": { description: "Environment File", icon: "file" },
      "properties": { description: "Properties File", icon: "file" },
      "plist": { description: "Property List", icon: "file" },
      "gitignore": { description: "Git Ignore", icon: "file" },
      "dockerignore": { description: "Docker Ignore", icon: "file" },
      "editorconfig": { description: "Editor Config", icon: "file" },
      "eslintrc": { description: "ESLint Config", icon: "file" },
      "prettierrc": { description: "Prettier Config", icon: "file" },
      
      // Web Files
      "html": { description: "HTML File", icon: "file" },
      "htm": { description: "HTML File", icon: "file" },
      "css": { description: "CSS Stylesheet", icon: "file" },
      "scss": { description: "SCSS Stylesheet", icon: "file" },
      "sass": { description: "Sass Stylesheet", icon: "file" },
      "less": { description: "Less Stylesheet", icon: "file" },
      "vue": { description: "Vue Component", icon: "file" },
      "svelte": { description: "Svelte Component", icon: "file" },
      
      // Vector Graphics
      "eps": { description: "Encapsulated PostScript", icon: "file" },
      "ai": { description: "Adobe Illustrator", icon: "file" },
      "sketch": { description: "Sketch File", icon: "file" },
      "fig": { description: "Figma File", icon: "file" },
      "xd": { description: "Adobe XD", icon: "file" },
      
      // Other
      "exe": { description: "Executable", icon: "file" },
      "msi": { description: "Windows Installer", icon: "file" },
      "app": { description: "macOS Application", icon: "file" },
      "ttf": { description: "TrueType Font", icon: "file" },
      "otf": { description: "OpenType Font", icon: "file" },
      "woff": { description: "Web Font", icon: "file" },
      "woff2": { description: "Web Font 2", icon: "file" },
      "eot": { description: "Embedded OpenType", icon: "file" },
      "ics": { description: "Calendar File", icon: "file" },
      "vcf": { description: "vCard Contact", icon: "file" },
      "torrent": { description: "Torrent File", icon: "file" },
      
      // Mini Apps (WebXDC)
      "xdc": { description: "Mini App", icon: "gift", isMiniApp: true }
    };
  
    // Normalize the extension to lowercase
    const normalizedExt = extension.toLowerCase();
    
    // Return the file type info if found, otherwise return default values
    return fileTypes[normalizedExt] || { description: "Unknown File", icon: "file-unknown" };
}

/**
 * Slide out an element with animation and remove it from document flow
 * @param {HTMLElement} element - The DOM element to slide out
 * @param {Object} options - Optional configuration
 * @param {string} options.animationClass - CSS class for animation (default: 'slideout-anim')
 * @param {number} options.delay - Delay before starting animation in ms (default: 0)
 * @param {boolean} options.removeAfter - Whether to set display:none after animation (default: true)
 * @returns {Promise} Resolves when animation completes
 */
function slideout(element, options = {}) {
    // Default options
    const {
        animationClass = 'slideout-anim',
        delay = 0,
        removeAfter = true
    } = options;

    return new Promise(resolve => {
        // Store the initial height before starting animation
        const initialHeight = element.offsetHeight;

        // Optional delay before starting the animation
        setTimeout(() => {
            // Set the initial height as a CSS variable
            element.style.setProperty('--initial-height', `${initialHeight}px`);

            // Start the animation
            element.classList.add(animationClass);

            // Handle animation completion
            element.addEventListener('animationend', () => {
                // Clean up after animation
                element.classList.remove(animationClass);
                element.style.removeProperty('--initial-height');

                // Optionally hide the element
                if (removeAfter) element.style.display = 'none';

                // Resolve the promise
                resolve();
            }, { once: true });
        }, delay);
    });
}

/**
 * Calculate Levenshtein distance between two strings
 * @param {string} str1 
 * @param {string} str2 
 * @returns {number} The edit distance
 */
function levenshteinDistance(str1, str2) {
    const len1 = str1.length;
    const len2 = str2.length;
    
    // Create a 2D array for dynamic programming
    const dp = Array(len1 + 1).fill(null).map(() => Array(len2 + 1).fill(0));
    
    // Initialize first row and column
    for (let i = 0; i <= len1; i++) dp[i][0] = i;
    for (let j = 0; j <= len2; j++) dp[0][j] = j;
    
    // Fill the dp table
    for (let i = 1; i <= len1; i++) {
        for (let j = 1; j <= len2; j++) {
            if (str1[i - 1] === str2[j - 1]) {
                dp[i][j] = dp[i - 1][j - 1];
            } else {
                dp[i][j] = 1 + Math.min(
                    dp[i - 1][j],     // deletion
                    dp[i][j - 1],     // insertion
                    dp[i - 1][j - 1]  // substitution
                );
            }
        }
    }
    
    return dp[len1][len2];
}

/**
 * Build an x.com Vector Invite intent URL
 * @param {string} inviteCode - The invite code to include in the post
 * @param {Array<string>} hashtags - The hashtags to include in the post
 * @param {string} via - The tagged "Posted via" account
 * @returns {string} An encoded x.com intent URL
 */
function buildXIntentUrl(inviteCode, hashtags = ['Vector', 'Privacy'], via = 'VectorPrivacy') {
    const baseUrl = 'https://x.com/intent/post';
    
    // Build tweet text with proper handling of special characters
    const tweetText = `🐇  Wake up, the Matrix has you... 🔐  Use my Vector Invite Code: ${inviteCode}`;
    
    // Create URLSearchParams for reliable encoding
    const params = new URLSearchParams({
        text: tweetText,
        via: via,
        hashtags: hashtags.join()
    });
    
    return `${baseUrl}?${params.toString()}`;
}

/**
 * Pauses execution for a specified amount of time.
 *
 * @param {number} ms - The number of milliseconds to sleep
 * @returns {Promise<void>} A promise that resolves after the specified delay
 */
function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

// Referral params are DELIBERATE — someone chose to share credit with someone
// else, and a messenger that strips them can't share a referral link at all.
// The line: a stable identifier saying WHO gets credit stays; a per-click
// fingerprint the ad network minted for this one click (cjevent, irclickid,
// fbclid) goes. Never add a name from here to TRACKING_PARAMS_GLOBAL.
const REFERRAL_PARAM_RE = /^(?:tag|partner|wmlspartner|campid|a(?:f|ff)id|affname|aff|aff_[a-z_]+|affiliate(?:_id)?|ref|refcode|ref_code|referral(?:_?code)?|invite(?:_?code)?|invited_by|via|promo(?:_?code)?|coupon|discount(?:_?code)?|sponsor|supporter|creator|epic_(?:affiliate|creator_id))$/i;

// Tracking parameters common to every site. Sources: AdGuard TrackParamFilter,
// ClearURLs, Brave's query filter. Hoisted to module scope: linkify runs this
// per message, and rebuilding the list per call showed up as pure waste.
const TRACKING_PARAMS_GLOBAL = [
  // Facebook/Meta
  'fbclid', 'fbadid', 'fb_action_ids', 'fb_action_types', 'fb_comment_id',
  'fb_ref', 'fb_source', 'action_object_map', 'action_type_map',
  'action_ref_map', 'mibextid', 'extid',

  // Google Ads / Shopping / Analytics
  'gclid', 'gclsrc', 'dclid', 'gbraid', 'wbraid', 'srsltid',
  'gad_campaignid', '_ga', '_gl', 'usqp',

  // Microsoft/Bing
  'msclkid',

  // Yandex
  'yclid', 'ysclid', 'clckid', '_openstat',

  // Twitter/X — bare 's'/'t' stay in the Twitter branch: globally they eat
  // WordPress search (?s=) and everyone else's timestamps (?t=).
  'twclid', '__twitter_impression',

  // TikTok / Twitch
  'ttclid', 'tt_medium', 'tt_content',

  // Reddit / Pinterest / LinkedIn ad click IDs
  'rdt_cid', 'epik', 'li_fat_id',

  // Per-click IDs minted by affiliate networks (Impact, CJ, Awin, ShareASale,
  // TradeDoubler, Rakuten). The affiliate's own ID is a referral, not these.
  'irclickid', 'irgwc', 'ir_campaignid', 'ir_adid', 'ir_partnerid',
  'cjevent', 'cjdata', 'awc', 'sscid', 'tduid', 'ranMID', 'ranEAID',
  'ranSiteID', 'clickid', 'iclid', 'external_click_id', 'rb_clickid',
  'wickedid', 'sms_click', 'sms_source', 'sms_uph',

  // Email + marketing automation
  'mc_cid', 'mc_eid', 'mc_tc', 'ck_subscriber_id', 'ml_subscriber',
  'ml_subscriber_hash', 'vero_conv', 'vero_id', 'mkt_tok', '__s',
  'elq', 'elqTrackId', 'elqaid', 'elqat', 'elqak',

  // HubSpot
  '_hsenc', '_hsmi', '__hssc', '__hstc', '__hsfp', 'hsCtaTracking',

  // Adobe / AT Internet / Webtrekk
  's_cid', 'sc_cid', 'adobe_mc_ref', 'adobe_mc_sdid', 'xtor',
  'wt_mc', 'wt_zmc', 'wtrid',

  // Publisher CMS campaign tags
  'cmpid', 'ncid', 'ftag', 'os_ehash', 'guccounter', 'guce_referrer',
  'guce_referrer_sig', 'tracking_source', 'recommended_by',

  // Alibaba's pair, pasted around by AliExpress/Taobao/Tmall/Lazada/Bilibili
  'spm', 'scm',

  // Generic tracking
  'referrer', 'source', 'campaign', 'medium'
];

// Whole `utm_` namespace, plus its clones under a vendor prefix (hmb_, mtm_,
// pk_, gad_, at_, int_, itm_) — matching the shape catches prefixes we've never
// seen. The ≤4-char prefix bound spares `search_term` / `custom_content`.
const TRACKING_UTM_CLONE_RE = /^(?:utm_.|[a-z][a-z0-9]{0,3}_(?:source|medium|campaign|term|content|source_platform|creative_format|marketing_tactic)$)/i;

// Namespaces a single analytics vendor owns outright, so nothing functional can
// live under them: Matomo, HubSpot Ads, AppsFlyer, Adjust, Branch, Blueshift,
// Sailthru, Vox, Vero, AT Internet's mail tags, Temu.
const TRACKING_NAMESPACE_RE = /^(?:mtm_|pk_|hsa_|af_|adj_|adjust_|_branch_|bsft_|_sgm_|oly_|vero_|_x_(?:ads|ns|bg|sessn)_|at_(?:campaign|creation|custom|emailtype|link|medium|ptr_|recipient|send_))/i;

/**
 * Removes tracking and marketing parameters from URLs for privacy.
 * Covers Big Tech, social, shopping, search engines and email campaign tags.
 * Referral/affiliate credit is preserved — see REFERRAL_PARAM_RE.
 *
 * @param {string} urlString - The URL to clean
 * @returns {string} The cleaned URL without tracking parameters
 */
function cleanTrackingFromUrl(urlString) {
  try {
    const url = new URL(urlString);
    const normalizedOriginal = url.href;
    const hostname = url.hostname.toLowerCase();
    // Raw pairs, captured before any mutation, so survivors can be restored
    // byte-exact at the end instead of re-serialized.
    const rawPairs = url.search ? url.search.slice(1).split('&') : [];
    const rawPairName = pair => {
      const raw = pair.split('=')[0];
      try { return decodeURIComponent(raw.replace(/\+/g, ' ')); } catch (e) { return raw; }
    };

    // Names actually present, so the ~200-entry lists cost a Set hit each rather
    // than a full delete() scan. Deleting never adds a name, so this stays valid.
    const present = new Set(url.searchParams.keys());
    const dropParams = names => names.forEach(name => {
      if (present.has(name)) url.searchParams.delete(name);
    });
    // Snapshot the keys: deleting from a live URLSearchParams skips entries.
    const dropMatching = (pattern, keep) => {
      for (const param of [...url.searchParams.keys()]) {
        if (pattern.test(param) && !(keep && keep.test(param))) url.searchParams.delete(param);
      }
    };
    const takeReferrals = () => [...url.searchParams].filter(([name]) => REFERRAL_PARAM_RE.test(name));
    // Anchored so `notamazon.evil.com` can't claim a host's rules. The label form
    // covers the multi-TLD giants (amazon.co.uk, google.de, ebay.com.au).
    const hostIs = (...domains) => domains.some(d => hostname === d || hostname.endsWith('.' + d));
    const hostLabelIs = label => new RegExp(`(^|\\.)${label}\\.[a-z]{2,}(\\.[a-z]{2,})?$`).test(hostname);

    // YouTube. `t`/`start`/`list`/`index`/`lc` are functional (timestamp,
    // playlist position, linked comment) and must survive.
    if (hostIs('youtube.com', 'youtu.be', 'youtube-nocookie.com')) {
      dropParams([
        'feature', 'si', 'is', 'app', 'kw', 'annotation_id', 'src_vid',
        'ab_channel', 'start_radio', 'rv', 'pp', 'themeRefresh',
        'source_ve_path', 'embeds_referring_origin', 'embeds_referring_euri',
        'embeds_euri', 'embeds_origin'
      ]);
    }

    // Google properties. Search links carry a dozen session/telemetry params;
    // `q`, `hl`, `tbm`, `tbs`, `num`, `start` and Maps' `data` survive.
    else if (hostLabelIs('google')) {
      dropParams([
        'ved', 'ei', 'sei', 'oq', 'aqs', 'sourceid', 'sxsrf', 'rlz', 'uact',
        'usg', 'sa', 'esrc', 'cd', 'cad', 'atyp', 'vet', 'je', 'dcr', 'dpr',
        'iflsig', 'fbs', 'ictx', 'cshid', 'sclient', '_u', 'site', 'ie',
        'pcampaignid', 'icid', 'original_referer'
      ]);
      dropMatching(/^(?:gs_|gws_|gfe_|bi[hw]$|btn|sca_(?:esv|upv)$)/);
    }

    // Bing. Note the uppercase spellings: param deletion is case-sensitive.
    else if (hostIs('bing.com')) {
      dropParams([
        'cvid', 'CVID', 'qs', 'sk', 'sc', 'sp', 'pq', 'form', 'FORM', 'PC',
        'ghsh', 'ghacc', 'ghpl', 'toWww', 'redig', 'ntref', 'ocid'
      ]);
    }

    // DuckDuckGo. `t` is the client tag, but only junk on a shared search URL —
    // elsewhere on the site it can be load-bearing, so require `q` alongside it.
    else if (hostIs('duckduckgo.com')) {
      dropParams(['atb', 'origin', 'from', 'vis', 'perf_id', 'vqd', 'ia_source']);
      if (url.searchParams.has('q')) url.searchParams.delete('t');
    }

    // Yandex
    else if (hostLabelIs('yandex')) {
      dropParams([
        'lr', 'redircnt', 'clid', 'banerid', 'suggest_reqid', 'did', 'msid',
        'persistent_id', 'from'
      ]);
    }

    // Amazon — `tag` is the Associates referral and survives the wipe below.
    else if (hostLabelIs('amazon')) {
      // Amazon URLs: keep only the essential product ID path
      // Format: /product-name/dp/PRODUCT_ID or /dp/PRODUCT_ID
      const pathMatch = url.pathname.match(/\/dp\/([A-Z0-9]+)/i);
      if (pathMatch) {
        // Reconstruct clean Amazon URL with just the product ID
        const referrals = takeReferrals();
        url.search = ''; // Remove all query parameters
        // Keep the path up to and including the product ID
        const dpIndex = url.pathname.indexOf('/dp/');
        if (dpIndex !== -1) {
          url.pathname = url.pathname.substring(0, dpIndex + 14); // /dp/ + 10 char ID
        }
        referrals.forEach(([name, value]) => url.searchParams.set(name, value));
      }
      // If no product ID found, just remove tracking params
      dropParams([
        'crid', 'dib', 'dib_tag', 'keywords', 'qid', 'sprefix', 'sr',
        'ie', 'psc', 'ref', 'ref_', 'linkCode', 'creative', 'creativeASIN',
        'ascsubtag', 'asc_campaign', 'asc_refurl', 'asc_source', 'content-id',
        'social_share', 'th', 'smid', 'refRID', 'rnid', 'camp', 'spIA',
        'qualifier', '_encoding', 'dchild', 'starsLeft', 'skipTwisterOG',
        'aaxitk', 'ms3_c', 'colid', 'coliid', 'twchReferral', 'ingress',
        'yTwchPos'
      ]);
      dropMatching(/^(?:p[fd]_rd_|__mk_|cv_ct_|sb-ci-|field-lbr_)/);
    }

    // eBay-specific tracking — item pages carry a wall of _trkparms / itmprp /
    // itmmeta / itmprp junk. Anchored host match (not loose .includes) because
    // this branch rewrites the path, so a false positive would mangle the URL.
    else if (hostLabelIs('ebay')) {
      // Item pages reduce to /itm/ITEM_ID. The ID is a long digit run, either
      // right after /itm/ or the trailing segment of a legacy title-slug URL.
      const itmMatch = url.pathname.match(/\/itm\/(?:.+\/)?(\d{6,})/);
      if (itmMatch) {
        // `var` pre-selects a SKU on multi-variation listings — functional, not
        // a tracker, so it survives the wipe, as does Partner Network `campid`.
        const variation = url.searchParams.get('var');
        const referrals = takeReferrals();
        url.search = '';
        url.hash = '';
        url.pathname = `/itm/${itmMatch[1]}`;
        if (variation) url.searchParams.set('var', variation);
        referrals.forEach(([name, value]) => url.searchParams.set(name, value));
      } else {
        // Non-item eBay URLs (search, store, etc.): strip the known trackers.
        dropParams([
          '_trkparms', '_trksid', 'itmprp', 'itmmeta', 'hash', 'amdata',
          'epid', '_from', 'mkcid', 'mkrid', 'toolid', 'customid',
          'mkevt', 'nordt', 'rt', 'ssspo', 'sssrc', 'ssuid', 'widget_ver'
        ]);
      }
    }

    // Etsy
    else if (hostIs('etsy.com')) {
      dropParams([
        'click_key', 'click_sum', 'organic_search_click', 'ref', 'frs', 'sts'
      ]);
      dropMatching(/^ga_/);
    }

    // AliExpress / Taobao / Tmall / Lazada — `spm`/`scm` are stripped globally,
    // `aff_*` is the affiliate's credit and stays.
    else if (hostLabelIs('aliexpress') || hostLabelIs('lazada') || hostIs('taobao.com', 'tmall.com')) {
      dropParams([
        'algo_pvid', 'algo_expid', 'algo_exp_id', 'curPageLogUid', 'pdp_npi',
        'ws_ab_test', 'btsid', 'gps-id', 'mall_affr', 'terminal_id',
        'utparam', 'utparam-url', 'scm_id', 'scm-url', 'sk', 'dp', 'cv',
        'pvid', 'ut_sk', 'ali_refid', 'ali_trackid', 'acm', 'abbucket',
        'abtest', 'trackInfo', 'impid', 'clickTrackInfo', 'ad_src', 'impsrc'
      ]);
    }

    // Walmart / Target / Best Buy / Newegg / Temu
    else if (hostIs('walmart.com')) {
      dropParams(['u1', 'from', 'sourceid', 'veh']);
      dropMatching(/^ath/);
    }
    else if (hostIs('target.com')) {
      dropParams([
        'CPNG', 'LID', 'LNM', 'DFA', 'fndsrc', 'adgroup', 'network',
        'device', 'location', 'targetid', 'ds_rl'
      ]);
    }
    else if (hostIs('bestbuy.com')) {
      dropParams(['acampID', 'mpid', 'intl', 'loc']);
    }
    else if (hostIs('newegg.com')) {
      dropParams(['ACRID', 'ASUBID', 'ASID', 'nm_mc', 'cm_mmc']);
    }
    else if (hostIs('temu.com')) {
      dropParams(['top_gallery_url', 'refer_page_name', 'refer_page_id', 'refer_page_sn']);
    }

    // Twitter/X
    else if (hostIs('twitter.com', 'x.com', 't.co')) {
      dropParams(['s', 't', 'cn', 'ref_src', 'refsrc', 'ref_url', 'src']);
    }

    // Facebook / Messenger
    else if (hostIs('facebook.com', 'fb.com', 'fb.me', 'm.me', 'messenger.com')) {
      dropParams([
        'sfnsn', 'rdid', 'paipv', '_rdr', 'rdc', 'rdr', '__tn__', '_nc_x',
        'comment_tracking', 'dti', 'eav', 'idorvanity', 'wtsid', 'ls_ref',
        'action_history', 'tracking', 'referral_story_type', 'video_source',
        'ftentidentifier', 'pageid', 'eid'
      ]);
      dropMatching(/^(?:hc_|__cft__|__xts__)/);
    }

    // Instagram
    else if (hostIs('instagram.com')) {
      dropParams(['igshid', 'igsh', 'ig_rid']);
    }

    // TikTok
    else if (hostIs('tiktok.com')) {
      dropParams([
        'is_from_webapp', 'is_copy_url', 'sender_device', 'sender_web_id',
        'web_id', 'refer', 'u_code', 'share_app_id', 'share_app_name',
        'share_link_id', 'share_item_id', 'share_iid', 'share_region',
        'social_share_type', 'embed_source', 'referer_url', 'referer_video_id',
        'trackParams', 'ug_btm', 'enter_from', 'preview_pb', 'sec_user_id',
        'user_id', '_r', '_t', '_d'
      ]);
    }

    // Reddit — the app's Branch keys arrive percent-encoded (`%24deep_link`), but
    // searchParams decodes names, so the `$` spelling is the one that matches.
    else if (hostIs('reddit.com', 'redd.it')) {
      dropParams([
        'share_id', 'correlation_id', 'rdt', 'ref_source', 'ref_campaign',
        'entry_point', 'target_user', 'post_index', 'post_fullname',
        '$deep_link', '$3p', '$original_url', '$android_deeplink_path'
      ]);
    }

    // LinkedIn
    else if (hostIs('linkedin.com')) {
      dropParams([
        'trk', 'trkInfo', 'trackingId', 'refId', 'originalSubdomain',
        'midToken', 'midSig', 'eid', 'courseClaim'
      ]);
      dropMatching(/^li[a-z]{2}$/);
    }

    // Pinterest / Snapchat
    else if (hostIs('pinterest.com', 'pin.it')) {
      dropParams(['nic', 'nic_v1', 'nic_v2', 'amp_client_id', 'mweb_unauth_id', 'sender']);
    }
    else if (hostIs('snapchat.com')) {
      dropParams(['sc_referrer', 'sc_ua', 'sc_ref']);
    }

    // Spotify — `context` survives: it decides which playlist/album a track
    // plays inside.
    else if (hostIs('spotify.com')) {
      dropParams(['si', 'nd', 'nid', 'sp_cid', 'dlsi', 'pi', 'referral']);
    }

    // Apple. `cid`/`ct`/`pt`/`app` are Apple's campaign tags but far too generic
    // to touch anywhere else, hence the host scope.
    else if (hostIs('apple.com')) {
      dropParams(['itsct', 'itscg', 'cid', 'ct', 'pt', 'app']);
      dropMatching(/^ign-itsc/);
    }

    // Twitch — tt_medium/tt_content are already global.
    else if (hostIs('twitch.tv')) {
      dropParams(['tt_email_id']);
    }

    // Steam / GOG / Epic / Humble. `snr` is a breadcrumb of where you clicked
    // from; Humble's hmb_* fall to the UTM-clone rule.
    else if (hostIs('steampowered.com', 'steamcommunity.com')) {
      dropParams(['snr', 'curator_clanid']);
    }
    else if (hostIs('gog.com')) {
      dropParams(['pp', 'track_click', 'link_id']);
    }
    else if (hostIs('epicgames.com')) {
      dropParams(['epic_gameId']);
    }
    else if (hostIs('humblebundle.com')) {
      dropParams(['mcID', 'linkID']);
    }

    // Netflix / IMDb
    else if (hostIs('netflix.com')) {
      dropParams(['trackId', 'tctx']);
    }
    else if (hostIs('imdb.com')) {
      dropParams(['ref_']);
      dropMatching(/^pf_rd_/);
    }

    // GitHub email-notification links
    else if (hostIs('github.com')) {
      dropParams(['email_token', 'email_source', 'notification_referrer_id']);
    }

    // Vimeo
    else if (hostIs('vimeo.com')) {
      dropParams(['share', 'fl', 'fe']);
    }

    // Every host: the exact-name list, then the pattern families. Referral params
    // are shielded from the patterns but NOT from the site branches above — those
    // are curated per host, where a name like `ref_` is known to be a breadcrumb.
    dropParams(TRACKING_PARAMS_GLOBAL);
    dropMatching(TRACKING_UTM_CLONE_RE, REFERRAL_PARAM_RE);
    dropMatching(TRACKING_NAMESPACE_RE, REFERRAL_PARAM_RE);

    // Restore the survivors verbatim. URLSearchParams re-serializes the whole
    // query (`!` -> `%21`, space -> `+`), and Maps' `data=` objects to that.
    // Falls back to the re-serialized form if a branch introduced a new param.
    const survivors = new Set([...url.searchParams.keys()]);
    const kept = rawPairs.filter(pair => survivors.has(rawPairName(pair)));
    if (new Set(kept.map(rawPairName)).size === survivors.size) url.search = kept.join('&');

    // Only return the cleaned URL if tracking params were actually removed
    // This avoids unwanted URL normalization (e.g., adding trailing slashes)
    if (url.href === normalizedOriginal) return urlString;
    return url.toString();
  } catch (e) {
    // If URL parsing fails, return original
    return urlString;
  }
}

/**
 * Detects URLs in text and makes them clickable links.
 * This function converts plain text URLs into clickable anchor tags.
 * SECURITY: Only processes text nodes, validates URLs, and uses textContent for safety.
 * PRIVACY: Strips tracking parameters from URLs before linking.
 *
 * @param {HTMLElement} element - The DOM element containing text to linkify
 */
function linkifyUrls(element) {
  // Strict URL regex pattern that matches http(s) URLs
  // Matches URLs starting with http:// or https:// and continuing until whitespace or end
  // Stops at whitespace, quotes, or angle brackets (common URL delimiters)
  const urlPattern = /(https?:\/\/[^\s<>"{}|\\^`\[\]]+)/gi;
  
  // Process all text nodes within the element
  const walker = document.createTreeWalker(
    element,
    NodeFilter.SHOW_TEXT,
    {
      acceptNode: function(node) {
        // Only accept text nodes that are NOT inside:
        // - anchor tags (already linked)
        // - code blocks (should remain literal)
        // - pre tags (should remain literal)
        let parent = node.parentElement;
        while (parent && parent !== element) {
          const tagName = parent.tagName;
          if (tagName === 'A' || tagName === 'CODE' || tagName === 'PRE') {
            return NodeFilter.FILTER_REJECT;
          }
          parent = parent.parentElement;
        }
        return NodeFilter.FILTER_ACCEPT;
      }
    },
    false
  );
  
  const textNodes = [];
  let node;
  
  // Collect all text nodes first (to avoid modifying while iterating)
  while (node = walker.nextNode()) {
    textNodes.push(node);
  }
  
  // Process each text node
  textNodes.forEach(textNode => {
    const text = textNode.textContent;
    
    // Check if the text contains any URLs
    if (!urlPattern.test(text)) return;
    
    // Reset regex lastIndex
    urlPattern.lastIndex = 0;
    
    // Create a temporary container
    const fragment = document.createDocumentFragment();
    let lastIndex = 0;
    
    let match;
    while ((match = urlPattern.exec(text)) !== null) {
      const originalUrl = match[0];
      const matchIndex = match.index;
      
      // Additional validation: ensure URL has valid structure
      try {
        // Trim trailing punctuation that's likely not part of the URL
        // (common in prose: "Check out https://example.com.")
        let url = originalUrl.replace(/[.,;:!?]+$/, '');
        // A trailing ')' is prose punctuation unless the URL itself opened a
        // paren (e.g. /wiki/Foo_(bar)) — parenthesized URLs and raw markdown
        // hrefs sit inside (...) constantly. Parens counted once up front
        // (punctuation trims can't remove any), and the strip capped: real
        // prose closes a couple of parens, while each strip reallocates the
        // string, so a crafted paren-flood must not become quadratic work.
        let parenOpens = (url.match(/\(/g) || []).length;
        let parenCloses = (url.match(/\)/g) || []).length;
        for (let strips = 0; strips < 8 && url.endsWith(')') && parenCloses > parenOpens; strips++) {
          url = url.slice(0, -1).replace(/[.,;:!?]+$/, '');
          parenCloses--;
        }

        // This will throw if URL is malformed
        const urlObj = new URL(url);
        
        // Only allow http and https protocols (security)
        if (urlObj.protocol !== 'http:' && urlObj.protocol !== 'https:') {
          continue;
        }
        
        // Clean tracking parameters for privacy (if enabled)
        const cleanUrl = fStripTrackingEnabled ? cleanTrackingFromUrl(url) : url;
        
        // Add text before the URL
        if (matchIndex > lastIndex) {
          fragment.appendChild(
            document.createTextNode(text.substring(lastIndex, matchIndex))
          );
        }
        
        // Create clickable link using textContent (not innerHTML) for safety
        const link = document.createElement('a');
        link.href = cleanUrl; // Use cleaned URL
        link.textContent = cleanUrl; // Display cleaned URL
        link.classList.add('linkified-url');
        link.target = '_blank';
        link.rel = 'noopener noreferrer';
        
        // Additional security: prevent javascript: and data: URLs
        // (belt and suspenders approach)
        if (link.protocol === 'http:' || link.protocol === 'https:') {
          fragment.appendChild(link);
        } else {
          // If somehow a bad URL got through, just add it as text
          fragment.appendChild(document.createTextNode(url));
        }
        
        // Consume only what the link kept; trimmed prose punctuation flows
        // back into the following text segment instead of vanishing.
        lastIndex = matchIndex + url.length;
      } catch (e) {
        // Invalid URL, skip it and continue
        continue;
      }
    }
    
    // Add remaining text after the last URL
    if (lastIndex < text.length) {
      fragment.appendChild(
        document.createTextNode(text.substring(lastIndex))
      );
    }
    
    // Only replace if we actually created links
    if (fragment.childNodes.length > 0) {
      textNode.parentNode.replaceChild(fragment, textNode);
    }
  });
}

/**
 * Supported image extensions for inline URL images
 */
const INLINE_IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'svg'];

/** Pre-compiled regex for detecting image URLs in text */
const IMAGE_URL_PATTERN = new RegExp(
    `https?://[^\\s<>"{}|\\\\^\`\\[\\]]+\\.(${INLINE_IMAGE_EXTENSIONS.join('|')})`,
    'i'
);


/**
 * Replace an inline image loading indicator with the actual image
 * @param {HTMLElement} indicator - The loading indicator element
 * @param {string} cachedPath - Path to the cached image file
 */
function replaceInlineImageIndicator(indicator, cachedPath) {
    // Get the original link (previous sibling)
    const link = indicator.previousElementSibling;
    if (!link || !link.classList.contains('linkified-url')) {
        indicator.remove();
        return;
    }

    // Get the original URL for extension extraction
    const originalUrl = indicator.dataset.url;

    // Convert cached path to displayable URL
    const assetUrl = convertFileSrc(cachedPath);

    // Create image container (same structure as attachment images)
    const imgContainer = document.createElement('div');
    imgContainer.className = 'inline-image-container';

    // Create the image element
    const img = document.createElement('img');
    img.className = 'inline-image';
    img.src = assetUrl;

    // Add load handler for scroll correction (same logic as attachment images)
    img.addEventListener('load', () => {
        // Auto-scroll to bottom if within 100ms of chat opening
        if (chatOpenTimestamp && Date.now() - chatOpenTimestamp < 100) {
            scrollToBottom(domChatMessages, false);
        } else {
            softChatScroll();
        }
    }, { once: true });

    // Add error handler to fall back to link
    img.addEventListener('error', () => {
        imgContainer.replaceWith(link.cloneNode(true));
    }, { once: true });

    // Attach image preview handler for click-to-zoom
    attachImagePreview(img);

    imgContainer.appendChild(img);

    // Add file extension badge (same as attachment images)
    const extension = getExtensionFromUrl(originalUrl);
    if (extension) {
        attachFileExtBadge(img, imgContainer, extension);
    }

    // Replace the link with the image container
    link.replaceWith(imgContainer);
    indicator.remove();

}

/**
 * Set up listeners for inline image events
 * - Progress events update the loading spinner
 * - Cached events replace ALL matching loading indicators with the image
 */
function setupInlineImageListeners() {
    // Progress updates - find ALL indicators with matching URL
    window.__TAURI__.event.listen('inline_image_progress', (event) => {
        const { url, progress } = event.payload;
        if (progress < 0) return;

        // Find ALL loading indicators for this URL
        const indicators = document.querySelectorAll(`.inline-image-loading[data-url="${CSS.escape(url)}"]`);
        const displayProgress = Math.max(5, progress);

        for (const indicator of indicators) {
            indicator.style.setProperty('--progress', `${displayProgress}%`);
        }
    });

    // Image cached (or failed) - replace/remove ALL loading indicators
    window.__TAURI__.event.listen('inline_image_cached', (event) => {
        const { url, path } = event.payload;
        const indicators = document.querySelectorAll(`.inline-image-loading[data-url="${CSS.escape(url)}"]`);

        if (path) {
            // Success - replace with actual image
            for (const indicator of indicators) {
                replaceInlineImageIndicator(indicator, path);
            }
        } else {
            // Failed - just remove the loading indicators (keep the link)
            for (const indicator of indicators) {
                indicator.remove();
            }
        }

        // Also resolve any <img> waiting on this URL via bindBackendCachedImg
        // (link-preview og:image / favicon ride the same cache pipeline)
        const pendingImgs = document.querySelectorAll(`img[data-pending-cache-url="${CSS.escape(url)}"]`);
        for (const img of pendingImgs) {
            delete img.dataset.pendingCacheUrl;
            if (path) img.src = convertFileSrc(path);
            else img.dispatchEvent(new Event('error'));
        }
    });
}

/**
 * Bind a REMOTE image URL to an <img> through the backend cache pipeline.
 *
 * The WebView has no Tor awareness — a raw remote `img.src` is a clearnet
 * fetch from the user's real IP. `cache_url_image` downloads via the
 * Tor-aware backend client (HTTPS-only, SSRF-guarded) and we render the
 * cached file. On failure the img's `error` event fires so existing
 * fallbacks (hide favicon, remove preview) behave as before.
 * @param {HTMLImageElement} img
 * @param {string} url - remote https URL (attacker-controlled is fine)
 */
function bindBackendCachedImg(img, url) {
    if (!url || typeof url !== 'string') {
        queueMicrotask(() => img.dispatchEvent(new Event('error')));
        return;
    }
    img.dataset.pendingCacheUrl = url;
    invoke('cache_url_image', { url }).then(path => {
        if (path) {
            delete img.dataset.pendingCacheUrl;
            img.src = convertFileSrc(path);
        }
        // null = download already in flight; the inline_image_cached event
        // listener above fills us in when it lands.
    }).catch(() => {
        delete img.dataset.pendingCacheUrl;
        img.dispatchEvent(new Event('error'));
    });
}

// Initialize the listeners when the module loads
setupInlineImageListeners();

/**
 * Check if text contains an image URL based on extension
 * Handles both clean URLs and text containing URLs
 * @param {string} text - URL or text containing a URL to check
 * @returns {boolean} - True if text contains an image URL
 */
function isImageUrl(text) {
    if (!text) return false;

    // Try parsing as a clean URL first
    try {
        const urlObj = new URL(text);
        const path = urlObj.pathname.toLowerCase();
        if (INLINE_IMAGE_EXTENSIONS.some(ext => path.endsWith('.' + ext))) {
            return true;
        }
    } catch (e) {
        // Not a clean URL, try extracting from text
    }
 
    // Check for image URL pattern in text (uses pre-compiled regex)
    return IMAGE_URL_PATTERN.test(text);
}

/**
 * Process inline image URLs in a message element
 * Finds links to images and replaces them with cached inline image previews
 * @param {HTMLElement} element - The message element to process (span inside p)
 */
async function processInlineImages(element) {
    // Skip if web previews (including inline images) are disabled
    if (!fWebPreviewsEnabled) return;

    // Find all linkified URLs that point to images
    const links = element.querySelectorAll('a.linkified-url');

    for (const link of links) {
        const url = link.href;

        // Skip if not an image URL
        if (!isImageUrl(url)) continue;

        // Skip if already processed
        if (link.dataset.inlineImageProcessed) continue;
        link.dataset.inlineImageProcessed = 'true';

        // Add loading indicator after the link with data-url for event-based updates
        const loadingIndicator = document.createElement('span');
        loadingIndicator.className = 'inline-image-loading';
        loadingIndicator.dataset.url = url;
        link.after(loadingIndicator);

        try {
            // Call Rust backend to cache the image (emits progress events)
            const cachedPath = await invoke('cache_url_image', { url });

            if (cachedPath) {
                // Image was cached immediately (already in cache or just downloaded)
                // Use the shared helper to replace indicator with image
                replaceInlineImageIndicator(loadingIndicator, cachedPath);
            }
            // If cachedPath is null, another download is in progress.
            // The inline_image_cached event will update ALL indicators when complete.
        } catch (e) {
            // If caching fails, remove indicator and leave the link as-is
            loadingIndicator.remove();
            console.warn('[InlineImages] Failed to cache image:', url, e);
        }
    }
}

/**
 * Get text content of an element excluding inline image containers
 * Uses efficient child walking instead of DOM cloning
 * @param {HTMLElement} element - The element to get text from
 * @returns {string} - Text content without image container content
 */
function getTextContentWithoutImages(element) {
    let text = '';
    for (const node of element.childNodes) {
        if (node.nodeType === Node.TEXT_NODE) {
            text += node.textContent;
        } else if (node.nodeType === Node.ELEMENT_NODE) {
            // Skip inline image containers
            if (!node.classList.contains('inline-image-container')) {
                text += getTextContentWithoutImages(node);
            }
        }
    }
    return text;
}

/** Build a clickable @name mention pill for an npub. */
function _buildMentionPill(npub, queueSync) {
    const profile = getProfile(npub);
    if (queueSync && !profile) {
        // Uncached tagged profile → fetch it; the profile_update handler refreshes this chip's name.
        invoke('queue_profile_sync', { npub, priority: 'high', forceRefresh: false }).catch(() => {});
    }
    const span = document.createElement('span');
    span.className = 'mention';
    span.setAttribute('data-npub', npub);
    span.textContent = '@' + getName(npub);
    span.addEventListener('click', (e) => {
        // Open the mini-profile (same popup as a name/avatar tap), not the full screen.
        // stopPropagation so the opening click doesn't hit the document outside-click
        // dismiss. Works even for an uncached npub — showMiniProfile fetches + placeholders.
        e.stopPropagation();
        showMiniProfile(npub, span);
    });
    return span;
}

/**
 * Replace @npub1... patterns in rendered message HTML with highlighted mention spans.
 * Uses a TreeWalker over text nodes (same approach as linkifyUrls).
 * @param {HTMLElement} element - The message span to process
 */
function renderMentions(element, senderIsAdmin = false, opts = {}) {
    // allowBare: also linkify a raw `npub1…` (optionally `@`/`nostr:`-prefixed) not preceded by a word
    // char or `/` — so npubs pasted plainly in a bio become tags. It also folds our OWN
    // vectorapp.io/profile/<npub> links into tags (that domain only; npubs in any other URL stay put).
    // The vectorapp.io branch is first so the bare branch's `/`-lookbehind can't swallow the npub
    // mid-URL. queueSync: fetch an uncached tagged profile so its name can fill in.
    const { allowBare = false, queueSync = false } = opts;
    const mentionPattern = allowBare
        ? /https?:\/\/vectorapp\.io\/profile\/(npub1[a-z0-9]{58})\b|(?<![\w/])(?:@|nostr:)?(npub1[a-z0-9]{58})\b/g
        : /@(npub1[a-z0-9]{58})/g;
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, {
        acceptNode(node) {
            // Skip text inside anchors, code blocks, or existing mention spans
            let parent = node.parentElement;
            while (parent && parent !== element) {
                const tag = parent.tagName;
                if (tag === 'A' || tag === 'CODE' || tag === 'PRE' || parent.classList.contains('mention')) {
                    return NodeFilter.FILTER_REJECT;
                }
                parent = parent.parentElement;
            }
            return NodeFilter.FILTER_ACCEPT;
        }
    });
    const textNodes = [];
    while (walker.nextNode()) textNodes.push(walker.currentNode);

    for (const node of textNodes) {
        if (!mentionPattern.test(node.textContent)) continue;
        mentionPattern.lastIndex = 0;

        const frag = document.createDocumentFragment();
        let lastIdx = 0;
        let match;
        while ((match = mentionPattern.exec(node.textContent)) !== null) {
            // Text before this match
            if (match.index > lastIdx) {
                frag.appendChild(document.createTextNode(node.textContent.slice(lastIdx, match.index)));
            }
            const npub = match[1] || match[2];   // group 1 = vectorapp.io link, group 2 = bare npub
            frag.appendChild(_buildMentionPill(npub, queueSync));
            lastIdx = match.index + match[0].length;
        }
        // Remaining text after last match
        if (lastIdx < node.textContent.length) {
            frag.appendChild(document.createTextNode(node.textContent.slice(lastIdx)));
        }
        node.parentNode.replaceChild(frag, node);
    }

    // Anchor pass: fold our OWN profile links that linkifyUrls (or markdown)
    // already turned into anchors. Only vectorapp.io/profile, only WYSIWYG
    // anchors (text == destination): a custom-labelled link keeps its text,
    // and a foreign-domain URL containing an npub is never carved up.
    if (allowBare) {
        const profileHref = /^https?:\/\/vectorapp\.io\/profile\/(npub1[a-z0-9]{58})\/?$/i;
        for (const a of element.querySelectorAll('a')) {
            const m = (a.getAttribute('href') || '').match(profileHref);
            if (!m) continue;
            if (!anchorShowsItsDestination(a)) continue;
            a.replaceWith(_buildMentionPill(m[1].toLowerCase(), queueSync));
        }
    }

    // Second pass: render @everyone for admin senders in group chats
    if (senderIsAdmin) {
        const everyonePattern = /@everyone\b/g;
        const walker2 = document.createTreeWalker(element, NodeFilter.SHOW_TEXT, {
            acceptNode(node) {
                let parent = node.parentElement;
                while (parent && parent !== element) {
                    const tag = parent.tagName;
                    if (tag === 'A' || tag === 'CODE' || tag === 'PRE' || parent.classList.contains('mention')) {
                        return NodeFilter.FILTER_REJECT;
                    }
                    parent = parent.parentElement;
                }
                return NodeFilter.FILTER_ACCEPT;
            }
        });
        const textNodes2 = [];
        while (walker2.nextNode()) textNodes2.push(walker2.currentNode);

        for (const node of textNodes2) {
            if (!everyonePattern.test(node.textContent)) continue;
            everyonePattern.lastIndex = 0;

            const frag = document.createDocumentFragment();
            let lastIdx = 0;
            let match;
            while ((match = everyonePattern.exec(node.textContent)) !== null) {
                if (match.index > lastIdx) {
                    frag.appendChild(document.createTextNode(node.textContent.slice(lastIdx, match.index)));
                }
                const span = document.createElement('span');
                span.className = 'mention mention-everyone';
                span.textContent = '@everyone';
                frag.appendChild(span);
                lastIdx = match.index + match[0].length;
            }
            if (lastIdx < node.textContent.length) {
                frag.appendChild(document.createTextNode(node.textContent.slice(lastIdx)));
            }
            node.parentNode.replaceChild(frag, node);
        }
    }
}
