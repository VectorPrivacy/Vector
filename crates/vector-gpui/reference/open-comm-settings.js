// Vector Devs' Community Settings; window.__SECTION__ names a section to open.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const row = [...document.querySelectorAll('.ws-rail-item')].find((e) => e.textContent.includes('Vector Devs'));
    row?.click();
    await sleep(600);
    const id = (window.__VECTOR_FIXTURES__.communities || [])[0].community_id;
    openCommunitySettings(id);
    await sleep(700);
    if (window.__SECTION__) [...document.querySelectorAll('button, [role=button], .sn-item, nav *')].find((e) => e.textContent.trim() === window.__SECTION__)?.click();
})();
