// Vector Devs' #dev Channel Settings; window.__TYPE__ names a section to open.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const row = [...document.querySelectorAll('.ws-rail-item')].find((e) => e.textContent.includes('Vector Devs'));
    row?.click();
    await sleep(600);
    const com = (window.__VECTOR_FIXTURES__.communities || [])[0];
    const ch = com.channels.find((c) => c.name === (window.__CHANNEL__ || 'dev'));
    openChannelEditorFor(com.community_id, { id: ch.channel_id, name: ch.name, topic: ch.topic, view: ch.view, private: ch.private });
    await sleep(700);
    if (window.__TYPE__) [...document.querySelectorAll('button, [role=button], .sn-item, nav *')].find((e) => e.textContent.trim() === window.__TYPE__)?.click();
    await sleep(300);
})();
