// Opens the chat named by OPEN, then expands its first Nostr article card.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => document.querySelector('.ne-btn-primary')?.click(), 1500);
})();
