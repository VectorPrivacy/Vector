// Opens OPEN and types decorated source into the composer: a tag, then each markdown style.
(async () => {
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    rows.find((r) => r.textContent.trim().toLowerCase().startsWith(window.__OPEN_NAME__))?.click();
    await new Promise((r) => setTimeout(r, 800));
    composerMentionLookup = () => [{ name: 'dave', npub: 'npub1v84qsqlcs56j8dmh6s22eccnpn2d87fdutxhl7rftseh67wzamhqfv9ft6' }];
    const input = domChatMessageInput;
    input.focus();
    input.value = 'Hey @dave **bold** _it_ ~~gone~~ `code` ||secret|| ok';
    input.dispatchEvent(new Event('input', { bubbles: true }));
})();
