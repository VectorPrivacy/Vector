// Opens the named chat (OPEN), the composer's emoji picker, then the pack creator:
// a new pack, or (CREATOR=edit) the first own pack's editor.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => document.getElementById('chat-input-emoji')?.click(), 600);
    setTimeout(() => {
        const edit = new URLSearchParams(location.search).get('creator') === 'edit' || window.__CREATOR__ === 'edit';
        (edit ? document.querySelector('.emoji-pack-edit-pencil') : document.querySelector('.emoji-pack-tab-create'))?.click();
    }, 1400);
})();
