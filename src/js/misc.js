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

    // Create a gradient for it using Purple and their personalised HEX
    divAvatar.style.background = `linear-gradient(-40deg, #${rHex}${gHex}${bHex}, 65%, purple)`;

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