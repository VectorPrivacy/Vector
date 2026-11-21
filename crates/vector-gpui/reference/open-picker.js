// Opens the named chat (OPEN), then the composer's emoji picker.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => document.getElementById('chat-input-emoji')?.click(), 600);
})();
