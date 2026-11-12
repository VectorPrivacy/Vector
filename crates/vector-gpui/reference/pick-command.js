// Opens the first chat, types window.__TYPE__ and presses Enter to pick the command it lists.
(async () => {
    document.querySelector('.chatlist-contact')?.click();
    await new Promise((r) => setTimeout(r, 800));
    const input = domChatMessageInput;
    input.focus();
    input.value = window.__TYPE__ || '';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((r) => setTimeout(r, 300));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
})();
