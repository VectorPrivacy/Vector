// Settings at Audio, scrolled to its Voice section.
(() => {
    setTimeout(() => openSettings(), 300);
    setTimeout(() => [...document.querySelectorAll('.cs-section')].find((b) => b.textContent.trim().startsWith('Audio'))?.click(), 800);
})();
