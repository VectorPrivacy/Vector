// Opens the chat named by window.__OPEN_NAME__ (set by capture.mjs from OPEN), else the first.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
})();
