// Opens the named chat (OPEN) and the send preview for a picture, with its compression estimate.
(() => {
    window.__FILE_SIZES__ = { 'photo.png': 170552 };
    window.__COMPRESSION__ = { original_size: 170552, estimated_size: 51743, savings_percent: 69 };
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => openFilePreview('media/photo.png', strOpenChat, ''), 300);
})();
