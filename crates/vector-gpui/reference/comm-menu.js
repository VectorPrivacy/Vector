// Opens Vector Devs, then its header menu; window.__PICK__ names a menu item to click after.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const row = [...document.querySelectorAll('.ws-rail-item')].find((e) => e.textContent.includes('Vector Devs'));
    row?.click();
    await sleep(600);
    document.querySelector('#chatlist-community-head')?.click();
    await sleep(500);
    if (window.__PICK__) {
        const item = [...document.querySelectorAll('.context-menu-item, .context-menu button, [role=menuitem]')].find((e) => e.textContent.trim().startsWith(window.__PICK__));
        for (const t of ['pointerdown', 'mousedown', 'pointerup', 'mouseup']) item?.dispatchEvent(new PointerEvent(t, { bubbles: true }));
        item?.click();
        await sleep(600);
    }
})();
