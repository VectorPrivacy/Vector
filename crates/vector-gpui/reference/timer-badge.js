// Opens OPEN with a Self-Destruct Timer on and a draft typed, so Send wears its badge.
(async () => {
    openChat(window.__OPEN_NAME__);
    await new Promise((r) => setTimeout(r, 800));
    const t = domChatMessageInput;
    t.focus();
    t.value = 'see you soon';
    t.selectionStart = t.selectionEnd = t.value.length;
    t.dispatchEvent(new Event('input', { bubbles: true }));
    updateSelfDestructIndicator(strOpenChat);
})();
