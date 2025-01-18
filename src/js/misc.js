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