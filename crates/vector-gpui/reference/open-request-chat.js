// A DM request opened from the Requests tab.
(() => {
    setTimeout(() => [...document.querySelectorAll('.cc-tab')].find((t) => t.textContent.includes('Requests'))?.click(), 300);
    setTimeout(() => document.querySelector('#chat-list .chatlist-contact')?.click(), 700);
})();
