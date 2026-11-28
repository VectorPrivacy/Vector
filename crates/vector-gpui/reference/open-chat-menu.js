// Opens OPEN and its header's ⋯ menu; TYPE=timer goes on to the Self-Destruct Timer picker.
(async () => {
    openChat(window.__OPEN_NAME__);
    await new Promise((r) => setTimeout(r, 600));
    document.querySelector('.nav-menu-btn')?.click();
    if (window.__TYPE__ !== 'timer') return;
    await new Promise((r) => setTimeout(r, 400));
    hideContextMenu?.();
    openSelfDestructPicker(strOpenChat, document.querySelector('.nav-menu-btn').getBoundingClientRect());
})();
