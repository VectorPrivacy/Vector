// Opens mo's chat, its attachment panel, then the PIVX wallet; PIVX_DIALOG clicks a dock button.
(() => {
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    rows.find((r) => r.textContent.trim().toLowerCase().startsWith('mo'))?.click();
    setTimeout(() => document.getElementById('chat-input-file')?.click(), 300);
    setTimeout(() => [...document.querySelectorAll('#attachment-panel button, #attachment-panel div')].find((e) => e.textContent.trim() === 'Mini Apps')?.click(), 700);
    setTimeout(() => document.getElementById('attachment-panel-pivx')?.click(), 1200);
    const d = window.__PIVX_DIALOG__;
    if (d) setTimeout(() => document.getElementById(`pivx-${d}-btn`)?.click(), 2200);
})();
