// Opens OPEN scrolled to its first message.
(async () => {
    openChat(window.__OPEN_NAME__);
    await new Promise((r) => setTimeout(r, 1200));
    document.querySelector('.dmsg-selfdestruct')?.closest('[id]')?.scrollIntoView({ block: 'center' });
})();
