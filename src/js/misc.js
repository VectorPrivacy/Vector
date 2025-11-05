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
        resolve(strInputPlaceholder ? domPopupInput.value : true);
    };

    // Event handler for the cancel click
    const onCancelClick = (resolve) => {
        domPopupConfirmBtn.removeEventListener('click', onConfirmClick);
        domPopupCancelBtn.removeEventListener('click', onCancelClick);
        domPopup.style.display = 'none';
        resolve(false);
    };

    // Create a promise that resolves when either the confirm or cancel button was clicked
    return new Promise((resolve) => {
        // Apply event listener for the confirm button
        domPopupConfirmBtn.addEventListener('click', () => onConfirmClick(resolve));

        // Apply event listener for the cancel button
        if (!fNotice) domPopupCancelBtn.addEventListener('click', () => onCancelClick(resolve));
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
      "gif": { description: "GIF Animation", icon: "video" },
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
      "mp4": { description: "Video", icon: "video" },
      "webm": { description: "Video", icon: "video" },
      "mov": { description: "Video", icon: "video" },
      "avi": { description: "Video", icon: "video" },
      "mkv": { description: "Video", icon: "video" },
      "flv": { description: "Flash Video", icon: "video" },
      "wmv": { description: "Windows Video", icon: "video" },
      "mpg": { description: "MPEG Video", icon: "video" },
      "mpeg": { description: "MPEG Video", icon: "video" },
      "m4v": { description: "MPEG-4 Video", icon: "video" },
      "3gp": { description: "3GP Video", icon: "video" },
      "3g2": { description: "3G2 Video", icon: "video" },
      "f4v": { description: "Flash MP4 Video", icon: "video" },
      "asf": { description: "Advanced Systems Format", icon: "video" },
      "rm": { description: "RealMedia", icon: "video" },
      "vob": { description: "DVD Video", icon: "video" },
      "ogv": { description: "Ogg Video", icon: "video" },
      "mxf": { description: "Material Exchange Format", icon: "video" },
      "ts": { description: "MPEG Transport Stream", icon: "video" },
      "m2ts": { description: "Blu-ray Video", icon: "video" },
      
      // Documents
      "pdf": { description: "PDF Document", icon: "attachment" },
      "doc": { description: "Word Document", icon: "attachment" },
      "docx": { description: "Word Document", icon: "attachment" },
      "xls": { description: "Excel Spreadsheet", icon: "attachment" },
      "xlsx": { description: "Excel Spreadsheet", icon: "attachment" },
      "ppt": { description: "PowerPoint Presentation", icon: "attachment" },
      "pptx": { description: "PowerPoint Presentation", icon: "attachment" },
      "odt": { description: "OpenDocument Text", icon: "attachment" },
      "ods": { description: "OpenDocument Spreadsheet", icon: "attachment" },
      "odp": { description: "OpenDocument Presentation", icon: "attachment" },
      "rtf": { description: "Rich Text Document", icon: "attachment" },
      "tex": { description: "LaTeX Document", icon: "attachment" },
      "pages": { description: "Pages Document", icon: "attachment" },
      "numbers": { description: "Numbers Spreadsheet", icon: "attachment" },
      "key": { description: "Keynote Presentation", icon: "attachment" },
      
      // Text Files
      "txt": { description: "Text File", icon: "attachment" },
      "md": { description: "Markdown", icon: "attachment" },
      "log": { description: "Log File", icon: "attachment" },
      "csv": { description: "CSV File", icon: "attachment" },
      "tsv": { description: "TSV File", icon: "attachment" },
      
      // Data Files
      "json": { description: "JSON File", icon: "attachment" },
      "xml": { description: "XML File", icon: "attachment" },
      "yaml": { description: "YAML File", icon: "attachment" },
      "yml": { description: "YAML File", icon: "attachment" },
      "toml": { description: "TOML File", icon: "attachment" },
      "sql": { description: "SQL File", icon: "attachment" },
      "db": { description: "Database File", icon: "attachment" },
      "sqlite": { description: "SQLite Database", icon: "attachment" },
      
      // Archives
      "zip": { description: "ZIP Archive", icon: "attachment" },
      "rar": { description: "RAR Archive", icon: "attachment" },
      "7z": { description: "7-Zip Archive", icon: "attachment" },
      "tar": { description: "TAR Archive", icon: "attachment" },
      "gz": { description: "GZip Archive", icon: "attachment" },
      "bz2": { description: "BZip2 Archive", icon: "attachment" },
      "xz": { description: "XZ Archive", icon: "attachment" },
      "tgz": { description: "Compressed TAR", icon: "attachment" },
      "tbz": { description: "Compressed TAR", icon: "attachment" },
      "txz": { description: "Compressed TAR", icon: "attachment" },
      "cab": { description: "Cabinet Archive", icon: "attachment" },
      "iso": { description: "Disc Image", icon: "attachment" },
      "dmg": { description: "macOS Disk Image", icon: "attachment" },
      "pkg": { description: "Package File", icon: "attachment" },
      "deb": { description: "Debian Package", icon: "attachment" },
      "rpm": { description: "RPM Package", icon: "attachment" },
      "apk": { description: "Android Package", icon: "attachment" },
      "ipa": { description: "iOS App", icon: "attachment" },
      "jar": { description: "Java Archive", icon: "attachment" },
      "war": { description: "Web Archive", icon: "attachment" },
      "ear": { description: "Enterprise Archive", icon: "attachment" },
      
      // 3D Files
      "obj": { description: "3D Object", icon: "attachment" },
      "fbx": { description: "Autodesk FBX", icon: "attachment" },
      "gltf": { description: "GL Transmission Format", icon: "attachment" },
      "glb": { description: "GL Binary", icon: "attachment" },
      "stl": { description: "Stereolithography", icon: "attachment" },
      "ply": { description: "Polygon File", icon: "attachment" },
      "dae": { description: "COLLADA", icon: "attachment" },
      "3ds": { description: "3D Studio", icon: "attachment" },
      "blend": { description: "Blender File", icon: "attachment" },
      "c4d": { description: "Cinema 4D", icon: "attachment" },
      "max": { description: "3ds Max", icon: "attachment" },
      "ma": { description: "Maya ASCII", icon: "attachment" },
      "mb": { description: "Maya Binary", icon: "attachment" },
      "usdz": { description: "Universal Scene", icon: "attachment" },
      
      // CAD Files
      "dwg": { description: "AutoCAD Drawing", icon: "attachment" },
      "dxf": { description: "Drawing Exchange", icon: "attachment" },
      "step": { description: "STEP CAD", icon: "attachment" },
      "stp": { description: "STEP CAD", icon: "attachment" },
      "iges": { description: "IGES CAD", icon: "attachment" },
      "igs": { description: "IGES CAD", icon: "attachment" },
      "sat": { description: "ACIS SAT", icon: "attachment" },
      "ipt": { description: "Inventor Part", icon: "attachment" },
      "iam": { description: "Inventor Assembly", icon: "attachment" },
      "prt": { description: "Part File", icon: "attachment" },
      "sldprt": { description: "SolidWorks Part", icon: "attachment" },
      "sldasm": { description: "SolidWorks Assembly", icon: "attachment" },
      "slddrw": { description: "SolidWorks Drawing", icon: "attachment" },
      "catpart": { description: "CATIA Part", icon: "attachment" },
      "catproduct": { description: "CATIA Product", icon: "attachment" },
      
      // Code Files
      "js": { description: "JavaScript", icon: "attachment" },
      "ts": { description: "TypeScript", icon: "attachment" },
      "jsx": { description: "React JSX", icon: "attachment" },
      "tsx": { description: "React TSX", icon: "attachment" },
      "py": { description: "Python", icon: "attachment" },
      "rs": { description: "Rust", icon: "attachment" },
      "go": { description: "Go", icon: "attachment" },
      "java": { description: "Java", icon: "attachment" },
      "kt": { description: "Kotlin", icon: "attachment" },
      "cpp": { description: "C++", icon: "attachment" },
      "cc": { description: "C++", icon: "attachment" },
      "cxx": { description: "C++", icon: "attachment" },
      "c": { description: "C", icon: "attachment" },
      "h": { description: "Header File", icon: "attachment" },
      "hpp": { description: "C++ Header", icon: "attachment" },
      "cs": { description: "C#", icon: "attachment" },
      "rb": { description: "Ruby", icon: "attachment" },
      "php": { description: "PHP", icon: "attachment" },
      "swift": { description: "Swift", icon: "attachment" },
      "m": { description: "Objective-C", icon: "attachment" },
      "mm": { description: "Objective-C++", icon: "attachment" },
      "lua": { description: "Lua", icon: "attachment" },
      "r": { description: "R Script", icon: "attachment" },
      "scala": { description: "Scala", icon: "attachment" },
      "clj": { description: "Clojure", icon: "attachment" },
      "dart": { description: "Dart", icon: "attachment" },
      "ex": { description: "Elixir", icon: "attachment" },
      "elm": { description: "Elm", icon: "attachment" },
      "erl": { description: "Erlang", icon: "attachment" },
      "fs": { description: "F#", icon: "attachment" },
      "hs": { description: "Haskell", icon: "attachment" },
      "jl": { description: "Julia", icon: "attachment" },
      "nim": { description: "Nim", icon: "attachment" },
      "pl": { description: "Perl", icon: "attachment" },
      "sh": { description: "Shell Script", icon: "attachment" },
      "bash": { description: "Bash Script", icon: "attachment" },
      "zsh": { description: "Zsh Script", icon: "attachment" },
      "fish": { description: "Fish Script", icon: "attachment" },
      "ps1": { description: "PowerShell", icon: "attachment" },
      "bat": { description: "Batch File", icon: "attachment" },
      "cmd": { description: "Command File", icon: "attachment" },
      "vb": { description: "Visual Basic", icon: "attachment" },
      "vbs": { description: "VBScript", icon: "attachment" },
      "asm": { description: "Assembly", icon: "attachment" },
      "s": { description: "Assembly", icon: "attachment" },
      
      // Config Files
      "ini": { description: "INI Config", icon: "attachment" },
      "cfg": { description: "Config File", icon: "attachment" },
      "conf": { description: "Config File", icon: "attachment" },
      "config": { description: "Config File", icon: "attachment" },
      "env": { description: "Environment File", icon: "attachment" },
      "properties": { description: "Properties File", icon: "attachment" },
      "plist": { description: "Property List", icon: "attachment" },
      "gitignore": { description: "Git Ignore", icon: "attachment" },
      "dockerignore": { description: "Docker Ignore", icon: "attachment" },
      "editorconfig": { description: "Editor Config", icon: "attachment" },
      "eslintrc": { description: "ESLint Config", icon: "attachment" },
      "prettierrc": { description: "Prettier Config", icon: "attachment" },
      
      // Web Files
      "html": { description: "HTML File", icon: "attachment" },
      "htm": { description: "HTML File", icon: "attachment" },
      "css": { description: "CSS Stylesheet", icon: "attachment" },
      "scss": { description: "SCSS Stylesheet", icon: "attachment" },
      "sass": { description: "Sass Stylesheet", icon: "attachment" },
      "less": { description: "Less Stylesheet", icon: "attachment" },
      "vue": { description: "Vue Component", icon: "attachment" },
      "svelte": { description: "Svelte Component", icon: "attachment" },
      
      // Vector Graphics
      "eps": { description: "Encapsulated PostScript", icon: "attachment" },
      "ai": { description: "Adobe Illustrator", icon: "attachment" },
      "sketch": { description: "Sketch File", icon: "attachment" },
      "fig": { description: "Figma File", icon: "attachment" },
      "xd": { description: "Adobe XD", icon: "attachment" },
      
      // Other
      "exe": { description: "Executable", icon: "attachment" },
      "msi": { description: "Windows Installer", icon: "attachment" },
      "app": { description: "macOS Application", icon: "attachment" },
      "ttf": { description: "TrueType Font", icon: "attachment" },
      "otf": { description: "OpenType Font", icon: "attachment" },
      "woff": { description: "Web Font", icon: "attachment" },
      "woff2": { description: "Web Font 2", icon: "attachment" },
      "eot": { description: "Embedded OpenType", icon: "attachment" },
      "ics": { description: "Calendar File", icon: "attachment" },
      "vcf": { description: "vCard Contact", icon: "attachment" },
      "torrent": { description: "Torrent File", icon: "attachment" }
    };
  
    // Normalize the extension to lowercase
    const normalizedExt = extension.toLowerCase();
    
    // Return the file type info if found, otherwise return default values
    return fileTypes[normalizedExt] || { description: "File", icon: "attachment" };
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