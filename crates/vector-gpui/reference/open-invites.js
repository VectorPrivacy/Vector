// The Requests tab with the fixtures' pending Community invite in it.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await sleep(300);
    await loadCommunityInvites();
    invitesChanged();
    [...document.querySelectorAll('.cc-tab')].find((t) => t.textContent.includes('Requests'))?.click();
})();
