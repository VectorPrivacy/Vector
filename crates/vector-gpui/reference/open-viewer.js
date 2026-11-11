// Opens the named chat (OPEN), reveals its spoiler picture, then views it full screen.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => document.querySelector('.spoiler-overlay')?.click(), 300);
    setTimeout(() => { const i = [...document.querySelectorAll('.dmsg-image-attachment')].pop(); if (i) openImageViewer(i.dataset.full || i.src); }, 600);
})();
