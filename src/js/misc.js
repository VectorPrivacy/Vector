/**
 * Detects npub (Nostr public key) or vectorapp.io profile links in text
 * @param {string} text - Text to search for npub or profile links
 * @returns {Object|null} - Detected npub info or null if none found
 */
function detectNostrProfile(text) {
    if (!text || text.length < 10) return null;
    
    // Pattern for raw npub (bech32 encoded public key)
    // npub1 + 58 characters of bech32 data = 63 total characters
    const npubPattern = /\b(npub1[a-z0-9]{58})\b/i;
    
    // Pattern for vectorapp.io profile links
    const profileLinkPattern = /https?:\/\/vectorapp\.io\/profile\/(npub1[a-z0-9]{58})/i;
    
    // First check for profile links (higher priority - more explicit intent)
    const linkMatch = text.match(profileLinkPattern);
    if (linkMatch) {
        const trimmedText = text.trim();
        const isAtEnd = trimmedText.endsWith(linkMatch[0]);
        const textWithoutNpub = isAtEnd ? trimmedText.slice(0, -linkMatch[0].length).trim() : null;
        return {
            npub: linkMatch[1].toLowerCase(),
            type: 'link',
            originalMatch: linkMatch[0],
            isAtEnd: isAtEnd,
            textWithoutNpub: textWithoutNpub
        };
    }
    
    // Then check for raw npub
    const npubMatch = text.match(npubPattern);
    if (npubMatch) {
        const trimmedText = text.trim();
        const isAtEnd = trimmedText.endsWith(npubMatch[0]);
        const textWithoutNpub = isAtEnd ? trimmedText.slice(0, -npubMatch[0].length).trim() : null;
        return {
            npub: npubMatch[1].toLowerCase(),
            type: 'npub',
            originalMatch: npubMatch[0],
            isAtEnd: isAtEnd,
            textWithoutNpub: textWithoutNpub
        };
    }
    
    return null;
}

/**
 * Renders a compact profile preview card for a detected npub
 * @param {Object} npubInfo - Detected npub info from detectNostrProfile
 * @param {Object|null} profile - Optional profile data if already available
 * @returns {HTMLDivElement} Profile preview element
 */
function renderNostrProfilePreview(npubInfo, profile = null, isOnlyNpub = false) {
    const divProfile = document.createElement('div');
    divProfile.classList.add('msg-profile-preview');
    divProfile.setAttribute('data-npub', npubInfo.npub);
    
    // If this is the only content in the message, remove top margin
    if (isOnlyNpub) {
        divProfile.style.marginTop = '0';
    }
    
    // Avatar container
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.classList.add('msg-profile-avatar');

    // Create avatar with fallback to placeholder on error
    const miscAvatarSrc = getProfileAvatarSrc(profile);
    const imgAvatar = createAvatarImg(miscAvatarSrc, 40, false);
    divAvatarContainer.appendChild(imgAvatar);
    
    // Info container (name + npub)
    const divInfo = document.createElement('div');
    divInfo.classList.add('msg-profile-info');
    
    // Display name
    const spanName = document.createElement('span');
    spanName.classList.add('msg-profile-name');
    spanName.textContent = profile?.nickname || profile?.name || npubInfo.npub.substring(0, 12) + '…';
    if (profile?.nickname || profile?.name) {
        // Will be twemojified by caller if needed
        spanName.setAttribute('data-twemoji', 'true');
    }
    divInfo.appendChild(spanName);
    
    // Full npub (CSS handles overflow/cutoff)
    const spanNpub = document.createElement('span');
    spanNpub.classList.add('msg-profile-npub');
    spanNpub.textContent = npubInfo.npub;
    divInfo.appendChild(spanNpub);

    // Button container for copy and open buttons
    const divButtons = document.createElement('div');
    divButtons.classList.add('msg-profile-buttons');
    
    // Copy npub button
    const btnCopy = document.createElement('button');
    btnCopy.classList.add('msg-profile-copy-btn');
    const copyIcon = document.createElement('span');
    copyIcon.classList.add('icon', 'icon-copy');
    btnCopy.appendChild(copyIcon);
    btnCopy.setAttribute('data-npub', npubInfo.npub);
    btnCopy.title = 'Copy npub';
    divButtons.appendChild(btnCopy);

    // Open Profile button
    const btnOpen = document.createElement('button');
    btnOpen.classList.add('msg-profile-btn', 'accept-btn');
    btnOpen.textContent = 'Open';
    btnOpen.setAttribute('data-npub', npubInfo.npub);
    divButtons.appendChild(btnOpen);
    
    // Assemble the preview
    divProfile.appendChild(divAvatarContainer);
    divProfile.appendChild(divInfo);
    divProfile.appendChild(divButtons);
    
    return divProfile;
}

/**
 * Updates a profile preview element with loaded profile data
 * @param {HTMLElement} previewElement - The profile preview element to update
 * @param {Object} profile - The loaded profile data
 */
function updateNostrProfilePreview(previewElement, profile) {
    if (!previewElement || !profile) return;

    // Update avatar
    const avatarContainer = previewElement.querySelector('.msg-profile-avatar');
    const updateAvatarSrc = getProfileAvatarSrc(profile);
    if (avatarContainer) {
        avatarContainer.innerHTML = '';
        const imgAvatar = createAvatarImg(updateAvatarSrc, 40, false);
        avatarContainer.appendChild(imgAvatar);
    }
    
    // Update name
    const nameSpan = previewElement.querySelector('.msg-profile-name');
    if (nameSpan && (profile.nickname || profile.name)) {
        nameSpan.textContent = profile.nickname || profile.name;
        nameSpan.setAttribute('data-twemoji', 'true');
        // Twemojify if the function is available
        twemojify(nameSpan);
    }
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
 * @param {String} strTitleClass - If specified, a CSS class to be added to the title element (e.g., 'text-gradient').
 * @return {Promise<Boolean>} - The Promise will resolve to 'true' if confirm button was clicked, otherwise 'false'.
 */
async function popupConfirm(strTitle, strSubtext, fNotice = false, strInputPlaceholder = '', strIcon = '', strTitleClass = '') {
    // Display the popup and render the UI
    domPopup.style.display = '';
    domPopupIcon.src = './icons/' + strIcon;
    domPopupIcon.style.display = strIcon ? '' : 'none';
    domPopupTitle.innerText = strTitle;
    // Clear any previous classes and add the new one if specified
    domPopupTitle.className = strTitleClass;
    domPopupSubtext.innerHTML = strSubtext;

    // Show the backdrop by adding the active class
    domApp.classList.add('active');

    // Adjust the 'Confirm' button if this is only a notice
    domPopupConfirmBtn.innerText = fNotice ? 'Okay!' : 'Confirm';
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

    // Event handler for the confirm click
    const onConfirmClick = (resolve) => {
        domPopupConfirmBtn.removeEventListener('click', onConfirmClick);
        domPopupCancelBtn.removeEventListener('click', onCancelClick);
        domPopup.style.display = 'none';
        domApp.classList.remove('active');
        resolve(strInputPlaceholder ? domPopupInput.value : true);
    };

    // Event handler for the cancel click
    const onCancelClick = (resolve) => {
        domPopupConfirmBtn.removeEventListener('click', onConfirmClick);
        domPopupCancelBtn.removeEventListener('click', onCancelClick);
        domPopup.style.display = 'none';
        domApp.classList.remove('active');
        resolve(false);
    };

    // Create a promise that resolves when either the confirm or cancel button was clicked
    return new Promise((resolve) => {
        // Keyboard event handler
        const onKeyDown = (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                document.removeEventListener('keydown', onKeyDown);
                onConfirmClick(resolve);
            } else if (e.key === 'Escape') {
                e.preventDefault();
                document.removeEventListener('keydown', onKeyDown);
                if (fNotice) {
                    onConfirmClick(resolve);
                } else {
                    onCancelClick(resolve);
                }
            }
        };

        // Apply keyboard event listener
        document.addEventListener('keydown', onKeyDown);

        // Apply event listener for the confirm button
        domPopupConfirmBtn.addEventListener('click', () => {
            document.removeEventListener('keydown', onKeyDown);
            onConfirmClick(resolve);
        });

        // Apply event listener for the cancel button
        if (!fNotice) domPopupCancelBtn.addEventListener('click', () => {
            document.removeEventListener('keydown', onKeyDown);
            onCancelClick(resolve);
        });
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
function createScrollHandler(scrollableDiv, bottomButton, options = {}) {
    const SCROLL_THRESHOLD = options.threshold ?? 250;
    const THROTTLE_TIME = options.throttleTime ?? 150;
    const SMOOTH_SCROLL = options.smoothScroll ?? true;

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
    const handleScroll = throttle(() => {
        const currentScrollTop = scrollableDiv.scrollTop;
        const maxScroll = scrollableDiv.scrollHeight - scrollableDiv.clientHeight;
        const distanceFromBottom = maxScroll - currentScrollTop;
        
        if (distanceFromBottom > SCROLL_THRESHOLD) {
            bottomButton.classList.add('visible');
        } else {
            bottomButton.classList.remove('visible');
        }
    }, THROTTLE_TIME);

    /**
     * Scrolls to bottom and hides the button
     * @private
     */
    const handleButtonClick = () => {
        scrollToBottom(scrollableDiv, SMOOTH_SCROLL);
        bottomButton.classList.remove('visible');
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
            await callback();
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
function formatBytes(bytes, decimals = 2) {
    if (bytes === 0) return '0 Bytes';
    
    const units = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB'];
    let unitIndex = 0;
    let value = bytes;
    
    while (value >= 1024 && unitIndex < units.length - 1) {
      value /= 1024;
      unitIndex++;
    }
    
    return value.toFixed(decimals).replace(/\.0+$|(\.[0-9]*[1-9])0+$/, '$1') + ' ' + units[unitIndex];
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
      "md": { description: "Markdown", icon: "file" },
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
      "zip": { description: "ZIP Archive", icon: "file" },
      "rar": { description: "RAR Archive", icon: "file" },
      "7z": { description: "7-Zip Archive", icon: "file" },
      "tar": { description: "TAR Archive", icon: "file" },
      "gz": { description: "GZip Archive", icon: "file" },
      "bz2": { description: "BZip2 Archive", icon: "file" },
      "xz": { description: "XZ Archive", icon: "file" },
      "tgz": { description: "Compressed TAR", icon: "file" },
      "tbz": { description: "Compressed TAR", icon: "file" },
      "txz": { description: "Compressed TAR", icon: "file" },
      "cab": { description: "Cabinet Archive", icon: "file" },
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

/**
 * Removes tracking and marketing parameters from URLs for privacy.
 * Supports major platforms: YouTube, Amazon, Facebook, Twitter/X, Google, etc.
 *
 * @param {string} urlString - The URL to clean
 * @returns {string} The cleaned URL without tracking parameters
 */
function cleanTrackingFromUrl(urlString) {
  try {
    const url = new URL(urlString);
    const hostname = url.hostname.toLowerCase();
    
    // Common tracking parameters across all sites
    const commonTrackingParams = [
      // Google Analytics & Marketing
      'utm_source', 'utm_medium', 'utm_campaign', 'utm_term', 'utm_content',
      'utm_id', 'utm_source_platform', 'utm_creative_format', 'utm_marketing_tactic',
      
      // Facebook/Meta
      'fbclid', 'fb_action_ids', 'fb_action_types', 'fb_ref', 'fb_source',
      
      // Google Click Identifier
      'gclid', 'gclsrc', 'dclid',
      
      // Microsoft/Bing
      'msclkid',
      
      // Twitter/X
      'twclid', 's', 't',
      
      // TikTok
      'tt_medium', 'tt_content',
      
      // Mailchimp
      'mc_cid', 'mc_eid',
      
      // HubSpot
      '_hsenc', '_hsmi', '__hssc', '__hstc', '__hsfp', 'hsCtaTracking',
      
      // Marketo
      'mkt_tok',
      
      // Adobe
      'sc_cid',
      
      // Generic tracking
      'ref', 'referrer', 'source', 'campaign', 'medium'
    ];
    
    // YouTube-specific tracking
    if (hostname.includes('youtube.com') || hostname.includes('youtu.be')) {
      const youtubeTrackingParams = [
        'feature', 'si', 'app', 'kw', 'annotation_id', 'src_vid',
        'ab_channel', 'start_radio', 'rv', 'pp'
      ];
      youtubeTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // Amazon-specific tracking
    else if (hostname.includes('amazon.')) {
      // Amazon URLs: keep only the essential product ID path
      // Format: /product-name/dp/PRODUCT_ID or /dp/PRODUCT_ID
      const pathMatch = url.pathname.match(/\/dp\/([A-Z0-9]+)/i);
      if (pathMatch) {
        // Reconstruct clean Amazon URL with just the product ID
        url.search = ''; // Remove all query parameters
        // Keep the path up to and including the product ID
        const dpIndex = url.pathname.indexOf('/dp/');
        if (dpIndex !== -1) {
          url.pathname = url.pathname.substring(0, dpIndex + 14); // /dp/ + 10 char ID
        }
      }
      // If no product ID found, just remove tracking params
      const amazonTrackingParams = [
        'crid', 'dib', 'dib_tag', 'keywords', 'qid', 'sprefix', 'sr',
        'ie', 'psc', 'pd_rd_i', 'pd_rd_r', 'pd_rd_w', 'pd_rd_wg',
        'pf_rd_i', 'pf_rd_m', 'pf_rd_p', 'pf_rd_r', 'pf_rd_s', 'pf_rd_t',
        'ref', 'ref_', 'tag', 'linkCode', 'creative', 'creativeASIN',
        'ascsubtag', 'asc_campaign', 'asc_refurl', 'asc_source'
      ];
      amazonTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // Twitter/X-specific
    else if (hostname.includes('twitter.com') || hostname.includes('x.com')) {
      const twitterTrackingParams = ['s', 't', 'ref_src', 'ref_url', 'src'];
      twitterTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // Facebook-specific
    else if (hostname.includes('facebook.com') || hostname.includes('fb.com')) {
      const facebookTrackingParams = [
        'fbclid', 'fb_action_ids', 'fb_action_types', 'fb_ref', 'fb_source',
        'action_object_map', 'action_type_map', 'action_ref_map'
      ];
      facebookTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // Instagram-specific
    else if (hostname.includes('instagram.com')) {
      const instagramTrackingParams = ['igshid', 'igsh'];
      instagramTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // LinkedIn-specific
    else if (hostname.includes('linkedin.com')) {
      const linkedinTrackingParams = ['trk', 'trkInfo', 'lipi', 'licu', 'originalSubdomain'];
      linkedinTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // Vimeo-specific
    else if (hostname.includes('vimeo.com')) {
      const vimeoTrackingParams = ['share', 'fl', 'fe'];
      vimeoTrackingParams.forEach(param => url.searchParams.delete(param));
    }
    
    // Remove common tracking parameters from all URLs
    // Exception: YouTube uses 't' for timestamps, so skip it for YouTube URLs
    commonTrackingParams.forEach(param => {
      if ((hostname.includes('youtube.com') || hostname.includes('youtu.be')) && param === 't') return;
      url.searchParams.delete(param);
    });
    
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
        
        // Use original URL length for tracking position (before cleaning/trimming)
        lastIndex = matchIndex + originalUrl.length;
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

    // Check if message is now image-only (no other text)
    const element = imgContainer.closest('.message-content, span');
    if (element) {
        const textContent = getTextContentWithoutImages(element);
        if (!textContent.trim()) {
            const pMessage = element.closest('p');
            if (pMessage) {
                pMessage.classList.add('no-background');
                pMessage.style.overflow = 'visible';
            }
        }
    }
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
    let processedImages = 0;

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
                processedImages++;
            }
            // If cachedPath is null, another download is in progress.
            // The inline_image_cached event will update ALL indicators when complete.
        } catch (e) {
            // If caching fails, remove indicator and leave the link as-is
            loadingIndicator.remove();
            console.warn('[InlineImages] Failed to cache image:', url, e);
        }
    }

    // If we processed images, check if the message is image-only (no other text)
    if (processedImages > 0) {
        // Get the text content excluding the image containers
        const textContent = getTextContentWithoutImages(element);

        if (!textContent.trim()) {
            // Message is image-only - remove bubble styling like attachments
            const pMessage = element.closest('p');
            if (pMessage) {
                pMessage.classList.add('no-background');
                pMessage.style.overflow = 'visible';

                // Float based on whether it's our message or theirs
                const msgContainer = pMessage.closest('.msg-mine, .msg-them');
                if (msgContainer) {
                    pMessage.style.float = msgContainer.classList.contains('msg-mine') ? 'right' : 'left';
                }
            }
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