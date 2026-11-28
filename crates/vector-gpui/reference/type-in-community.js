// Opens the fixture community, then types window.__TYPE__ into its composer.
(() => {
    setTimeout(() => [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.textContent.trim() === 'Vector Devs')?.closest('[class*="rail"], div')?.click(), 400);
    setTimeout(() => {
        const t = domChatMessageInput;
        t.focus();
        t.value = window.__TYPE__ || '@';
        t.selectionStart = t.selectionEnd = t.value.length;
        t.dispatchEvent(new Event('input', { bubbles: true }));
    }, 1800);
})();
