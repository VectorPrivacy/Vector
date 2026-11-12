// Opens the fixture community, then its Lounge voice channel (run with VOICE_DEMO=present|joined).
(() => {
    setTimeout(() => [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.textContent.trim() === 'Vector Devs')?.closest('[class*="rail"], div')?.click(), 400);
    setTimeout(() => [...document.querySelectorAll('.chatlist-channel-name')].find((e) => e.textContent.trim() === 'Lounge')?.click(), 1100);
})();
