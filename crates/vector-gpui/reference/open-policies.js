// The Policies tab; window.__PRESET__ opens that template (or 'defaults').
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const row = [...document.querySelectorAll('.ws-rail-item')].find((e) => e.textContent.includes('Vector Devs'));
    row?.click();
    await sleep(600);
    const id = (window.__VECTOR_FIXTURES__.communities || [])[0].community_id;
    openModerationPanel(id);
    await sleep(500);
    [...document.querySelectorAll('.mod-tab, [class*="mod-tab"]')].find((e) => e.textContent.trim() === 'Policies')?.click();
    await sleep(500);
    const p = window.__PRESET__;
    if (p === 'defaults') [...document.querySelectorAll('button')].find((b) => b.textContent.includes('See the rules'))?.click();
    else if (p) [...document.querySelectorAll('.pol-card')].find((c) => c.textContent.includes(p))?.click();
})();
