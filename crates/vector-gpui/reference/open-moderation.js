// Vector Devs' Moderation console.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const row = [...document.querySelectorAll('.ws-rail-item')].find((e) => e.textContent.includes('Vector Devs'));
    row?.click();
    await sleep(600);
    const id = (window.__VECTOR_FIXTURES__.communities || [])[0].community_id;
    openModerationPanel(id);
})();
