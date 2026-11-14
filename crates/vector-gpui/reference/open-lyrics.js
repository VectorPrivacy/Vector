// Opens OPEN and the first lyrics sheet in it.
(async () => {
    openChat(window.__OPEN_NAME__);
    for (let i = 0; i < 40 && !document.querySelector('[aria-label="Show lyrics"]'); i++) await new Promise((r) => setTimeout(r, 50));
    document.querySelector('[aria-label="Show lyrics"]')?.click();
})();
