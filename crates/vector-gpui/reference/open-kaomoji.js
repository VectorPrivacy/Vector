// Opens the named chat (OPEN), the composer's emoji picker, then its kaomoji mode.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => document.getElementById('chat-input-emoji')?.click(), 600);
    setTimeout(() => document.querySelector('.picker-mode-btn[data-mode="kaomoji"]')?.click(), 1000);
})();
