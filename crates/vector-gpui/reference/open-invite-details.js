// The fixtures' pending Community invite, opened from the Requests tab.
(async () => {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await sleep(300);
    await loadCommunityInvites();
    invitesChanged();
    [...document.querySelectorAll('.cc-tab')].find((t) => t.textContent.includes('Requests'))?.click();
    await sleep(400);
    document.querySelector('.chatlist-invite')?.click();
})();
