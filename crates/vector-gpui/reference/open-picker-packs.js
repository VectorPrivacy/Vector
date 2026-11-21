// Opens the named chat (OPEN), the composer's emoji picker, then jumps to its first pack.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => document.getElementById('chat-input-emoji')?.click(), 600);
    setTimeout(() => document.querySelector('.emoji-pack-tab')?.click(), 1400);
})();
