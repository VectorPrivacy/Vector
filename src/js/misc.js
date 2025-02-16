/**
 * Generate a consistent Gradient Avatar from an npub
 * @param {string} npub - The npub to generate an avatar for
 * @param {string} username - A username to display initials from
 */
function pubkeyToAvatar(npub, username) {
    // Otherwise, display their Gradient Avatar
    const divAvatar = document.createElement('div');
    divAvatar.classList.add('placeholder-avatar');

    // Convert the last three chars of their npub in to RGB HEX as a placeholder avatar
    const strLastChars = npub.slice(-3).padEnd(3, 'a');
    const rHex = strLastChars[0].charCodeAt(0).toString(16).padStart(2, '0');
    const gHex = strLastChars[1].charCodeAt(0).toString(16).padStart(2, '0');
    const bHex = strLastChars[2].charCodeAt(0).toString(16).padStart(2, '0');

    // Create a gradient for it using Vector Green and their personalised HEX
    divAvatar.style.background = `linear-gradient(-40deg, #${rHex}${gHex}${bHex}, 75%, #59fcb3)`;

    // If a username is given, extract Initials or First Letter to be added on-top
    if (username) {
        const pInitials = document.createElement('p');
        pInitials.textContent = getNameInitials(username) || username[0].toUpperCase();
        divAvatar.appendChild(pInitials);
    }

    return divAvatar;
}

/**
 * Extract up to three initials from a name, for example: "JSKitty" -> "JSK"
 * or "Michael Jackson" -> "MJ".
 * @param {string} str - A username to extract name initials from
 * @returns {string} - Up to three initials
 */
const getNameInitials = str => (str.match(/[A-Z]/g) || []).slice(0, 3).join('');

/**
 * Show a popup dialog to confirm an action.
 *
 * @param {String} strTitle - The title of the popup dialog.
 * @param {String} strSubtext - The subtext of the popup dialog.
 * @param {Boolean} fNotice - If this is a Notice or an Interactive Dialog.
 * @param {String} strInputPlaceholder - If specified, renders a text input with a custom placeholder, and returns a string instead of a boolean.
 * @return {Promise<Boolean>} - The Promise will resolve to 'true' if confirm button was clicked, otherwise 'false'.
 */
async function popupConfirm(strTitle, strSubtext, fNotice = false, strInputPlaceholder = '') {
    // Display the popup and render the UI
    domPopup.style.display = '';
    domPopupTitle.innerText = strTitle;
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
        resolve(strInputPlaceholder ? '' : false);
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