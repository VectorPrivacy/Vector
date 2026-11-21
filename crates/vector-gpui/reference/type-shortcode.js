// Opens the chat named by OPEN, then types a :shortcode into its composer as a user would.
(() => {
    const want = window.__OPEN_NAME__;
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    (want ? rows.find((r) => r.textContent.trim().toLowerCase().startsWith(want)) : rows[0])?.click();
    setTimeout(() => {
        const t = domChatMessageInput;
        t.focus();
        t.value = window.__TYPE__ || 'hey :sm';
        t.selectionStart = t.selectionEnd = t.value.length;
        t.dispatchEvent(new Event('input', { bubbles: true }));
    }, 1200);
})();
