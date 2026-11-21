// Settings at the category CAT names (by its nav label).
(() => {
    setTimeout(() => openSettings(), 300);
    setTimeout(() => [...document.querySelectorAll('.cs-section')].find((b) => b.textContent.trim().startsWith(window.__CAT__))?.click(), 800);
})();
