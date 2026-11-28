// Pulls past the top of the chat list to reveal Archived Chats, then unfolds it (run with CHATLIST_DEMO=1).
(() => {
    const list = document.querySelector('#chat-list');
    list?.dispatchEvent(new WheelEvent('wheel', { deltaY: -200, bubbles: true }));
    setTimeout(() => document.querySelector('.chatlist-archive')?.click(), 500);
})();
