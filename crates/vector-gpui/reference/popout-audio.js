// Plays OPEN's audio number TYPE (from 0), then opens mo so it floats.
(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    openChat(window.__OPEN_NAME__);
    for (let i = 0; i < 40 && !document.querySelector('.audio-play-btn'); i++) await wait(50);
    document.querySelectorAll('.audio-play-btn')[+window.__TYPE__ || 0]?.click();
    await wait(400);
    openChat('npub17jjvuhaxxs9rt2jakz6td5c6d7a2vpfr2erqmwc9xajhmqplt03q63stmv');
})();
