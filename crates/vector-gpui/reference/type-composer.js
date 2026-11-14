// Opens the first chat and types window.__TYPE__ into the composer, as a user would.
(async () => {
    window.__BADGES__ = { vector: true, vector_2026: true, tier: 3, bug_hunter: 0 };
    document.querySelector('.chatlist-contact')?.click();
    await new Promise((r) => setTimeout(r, 800));
    const input = domChatMessageInput;
    input.focus();
    input.value = window.__TYPE__ || '';
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new Event('input', { bubbles: true }));
})();
